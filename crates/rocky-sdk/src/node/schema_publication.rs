use super::*;
use crate::{
    schema::{MigrationStep, Schema},
    tip_schemas::Record,
};
use rocky_credentials::{
    SEAL_FORMAT, SealAction, SealStatement, SignedSeal, SignedTipOpening, TIP_OPENING_FORMAT,
    TipOpeningStatement,
};
use rocky_secure::{SchemaStage, TipStatement};
use std::collections::BTreeSet;

impl Node {
    pub(crate) async fn publish_schema_at_open(
        &self,
        confirmation: Option<&str>,
    ) -> Result<(), Failure> {
        if self.replica.staged_schema().await?.is_some() {
            self.follow_schema().await?;
            self.run_staged_migration().await?;
            return self.complete_rights_change().await;
        }
        self.complete_rights_change().await?;
        let Some((scope, bookmark)) = self.replica.followed_bookmark().await else {
            return Ok(());
        };
        let settings = self.settings.lock().await.clone();
        if settings.application(&self.declared)? != crate::DeclaredSchema::Ahead {
            return Ok(());
        }
        let Ok(signer) = self.tip_signer(scope).await else {
            return Ok(());
        };
        let source = self.replica.followed_tip().await;
        let graph = self.replica.tip_graph(scope).await?;
        if graph.bookmark(&bookmark)? != Some(source) || graph.seal(source)?.is_some() {
            return Ok(());
        }
        let narrowed = self.role_changes(&settings).await?;
        crate::rollout::confirm(&settings, &self.path, &self.declared, confirmation).await?;
        let migrates = crate::generation::migrates(&self.declared, settings.generation);
        if migrates && !settings.may_migrate(&self.domain())? {
            return Ok(());
        }
        let domain = crate::domain::Domain::at(
            &self.declared,
            *settings.publisher.as_bytes(),
            settings.mesh,
            settings.domain,
            self.declared.version,
            rocky_core::EngineIdentity::current(),
        )?;
        let signer = self.hold_stage_chain(signer, scope).await?;
        let seal = signer.endorse::<SignedSeal>(SealStatement {
            format: SEAL_FORMAT,
            scope,
            tip: source,
            frontier: self.replica.seal_frontier(scope, migrates).await?,
            action: if migrates {
                SealAction::Close
            } else {
                SealAction::Continue {
                    schema: domain.manifest().hash(),
                }
            },
        })?;
        let mut statements = vec![TipStatement::Seal(seal.clone())];
        let target = if migrates {
            let opening = signer.endorse::<SignedTipOpening>(TipOpeningStatement {
                format: TIP_OPENING_FORMAT,
                scope,
                seal: seal.id()?,
                schema: domain.manifest().hash(),
            })?;
            let target = opening.tip()?;
            statements.push(TipStatement::Opening(opening));
            target
        } else {
            seal.successor()?.ok_or("continuation has no successor")?
        };
        statements.push(Self::bookmark_move(
            &signer,
            scope,
            &bookmark,
            Some(source),
            Some(target),
        )?);
        let mut parents = BTreeSet::new();
        for migration in self.declared.migrations.iter().filter(|migration| {
            migration.version > settings.generation && migration.version <= self.declared.version
        }) {
            for step in &migration.statements {
                let sql = match step {
                    MigrationStep::Sql(sql) => sql,
                    MigrationStep::Insert { select, .. } => select,
                };
                let analyzed = rocky_storage::statement::analyze_migration(sql, |name| {
                    name.strip_prefix("parent.").map(str::to_owned)
                })
                .map_err(|error| format!("migration statement: {error:?}"))?;
                parents.extend(
                    analyzed
                        .tables
                        .into_iter()
                        .filter_map(|name| name.strip_prefix("parent.").map(str::to_owned)),
                );
            }
        }
        let publication = rocky_core::commitment("rocky.sdk.migration.v3", &target.0);
        self.change_rights_after(
            Some(publication),
            self.declared.version,
            &narrowed,
            super::people::Reach::Narrower,
        )
        .await?;
        self.check_fault(crate::database::fault::Stage::RightsChangeRecorded)?;
        self.replica
            .stage_schema(SchemaStage {
                publication,
                scope,
                bookmark,
                source,
                target,
                from_version: settings.generation,
                statements,
                program: serde_json::to_vec(&self.declared)?,
                parents: parents.into_iter().collect(),
            })
            .await?;
        drop(signer);
        self.replica
            .install_tip_schema(
                target,
                domain.manifest(),
                Record::new(
                    &self.declared,
                    self.declared.version,
                    rocky_core::EngineIdentity::current(),
                )
                .bytes()?,
            )
            .await?;
        self.follow_schema().await?;
        self.run_staged_migration().await?;
        self.check_fault(crate::database::fault::Stage::RightsChangeCommitted)?;
        self.complete_rights_change().await?;
        self.base_removal().await
    }

    pub(super) async fn base_removal(&self) -> Result<(), Failure> {
        let scope = self.domain().scope;
        if !self.replica.removal_unbased(scope).await? || self.tip_signer(scope).await.is_err() {
            return Ok(());
        }
        self.replica.finalize_path(scope).await?;
        self.replica.compact(scope, 0).await?;
        self.list_checkpoints(scope).await?;
        self.observations.touch();
        Ok(())
    }

    async fn role_changes(&self, settings: &Settings) -> Result<Vec<crate::PersonId>, Failure> {
        let current = settings.domain()?.logical().clone();
        let target = crate::domain::logical(&self.declared, *settings.publisher.as_bytes())?;
        let registry = self.registry(settings).await?;
        for role in crate::rollout::removed_roles(&current, &target) {
            let people = Self::role_holders(settings, &registry, &role);
            if !people.is_empty() {
                return Err(Box::new(crate::RoleInUse { role, people }));
            }
        }
        Ok(crate::rollout::narrowed_roles(&current, &target)
            .iter()
            .flat_map(|role| Self::role_holders(settings, &registry, role))
            .collect())
    }

    pub(super) async fn close_migrations(&self) -> Result<(), Failure> {
        let publications = self.replica.publications().await?;
        if publications.is_empty()
            || publications
                .iter()
                .any(|publication| publication.published < publication.bundles)
        {
            return Ok(());
        }
        let scope = self.domain().scope;
        let graph = self.replica.tip_graph(scope).await?;
        let followed = self.replica.followed_tip().await;
        let must_first_adopt_bookmark_winner = match self.replica.followed_bookmark().await {
            Some((_, name)) => graph.bookmark(&name)? != Some(followed),
            None => false,
        };
        if must_first_adopt_bookmark_winner {
            return Ok(());
        }
        let mut pending = false;
        let mut needs_seal = false;
        for publication in publications {
            if self
                .replica
                .migration_checkpoint_listed(publication.id)
                .await?
            {
                continue;
            }
            let Some(migration) = self.replica.migration_publication(publication.id).await? else {
                continue;
            };
            if migration.statement.tip != followed
                && !graph.succeeds(migration.statement.tip, followed)?
            {
                continue;
            }
            pending = true;
            let mut covered = false;
            for seal in &graph.seals {
                if seal.successor()? != Some(followed) {
                    continue;
                }
                if graph.schema(seal.statement.tip).is_err() {
                    continue;
                }
                if seal.statement.action
                    != (SealAction::Continue {
                        schema: graph.schema(seal.statement.tip)?,
                    })
                {
                    continue;
                }
                if graph
                    .seal(seal.statement.tip)?
                    .is_some_and(|winner| winner.id().ok() == seal.id().ok())
                    && (migration.statement.tip == seal.statement.tip
                        || graph.succeeds(migration.statement.tip, seal.statement.tip)?)
                    && (seal.statement.tip == followed
                        || graph.succeeds(seal.statement.tip, followed)?)
                {
                    covered = true;
                    break;
                }
            }
            needs_seal |= !covered;
        }
        if !pending {
            return Ok(());
        }
        if needs_seal {
            self.seal_followed(scope).await?;
        }
        self.replica.finalize_path(scope).await?;
        self.replica.compact(scope, 0).await?;
        self.list_checkpoints(scope).await?;
        self.observations.touch();
        Ok(())
    }

    async fn run_staged_migration(&self) -> Result<(), Failure> {
        let Some((stage, committed)) = self.replica.staged_schema().await? else {
            return Ok(());
        };
        if committed {
            return self.endorse_migration(&stage).await;
        }
        self.ensure_tip_node().await?;
        if self.replica.publication_staged(stage.publication).await? {
            self.replica
                .author_staged_publication(stage.publication)
                .await?;
            return self.endorse_migration(&stage).await;
        }
        let program = Schema::parse(std::str::from_utf8(&stage.program)?)?;
        let scopes = {
            let settings = self.settings.lock().await;
            let domain = self.domain();
            settings
                .granted(&domain)
                .into_iter()
                .filter(|scope| *scope != domain.registry())
                .collect()
        };
        let sql = self
            .replica
            .session(scopes)
            .await?
            .migration_sql(
                stage.publication,
                rocky_storage::sql::SqlOptions {
                    captured_rows: usize::MAX,
                    captured_bytes: usize::MAX,
                    statement_budget: rocky_storage::sql::MAX_TRANSACTION_TIMEOUT,
                    transaction_timeout: rocky_storage::sql::MAX_TRANSACTION_TIMEOUT,
                    ..Default::default()
                },
            )
            .await?;
        sql.run("BEGIN", vec![]).await?;
        for migration in program.migrations.iter().filter(|migration| {
            migration.version > stage.from_version && migration.version <= program.version
        }) {
            for (number, step) in migration.statements.iter().enumerate() {
                let ran = match step {
                    MigrationStep::Sql(statement) => sql.run(statement, vec![]).await.map(|_| ()),
                    MigrationStep::Insert { insert, select } => {
                        crate::database::insert_rows(&sql, self.domain().logical(), insert, select)
                            .await
                    }
                };
                if let Err(error) = ran {
                    sql.cancel().await;
                    drop(sql);
                    self.withdraw_stage(&stage).await?;
                    return Err(format!(
                        "migration to version {} failed at statement {}: {error}",
                        migration.version,
                        number + 1
                    )
                    .into());
                }
            }
        }
        sql.commit_publication(stage.publication).await?;
        drop(sql);
        self.replica
            .author_staged_publication(stage.publication)
            .await?;
        self.endorse_migration(&stage).await
    }

    pub(super) async fn release_committed_stage(&self) -> Result<bool, Failure> {
        match self.replica.staged_schema().await? {
            None => Ok(true),
            Some((stage, true)) => {
                self.endorse_migration(&stage).await?;
                Ok(true)
            }
            Some((_, false)) => Ok(false),
        }
    }

    async fn endorse_migration(&self, stage: &SchemaStage) -> Result<(), Failure> {
        if let Some((statement, writer)) = self.replica.migration_intent(stage.publication).await? {
            let signer = self.tip_signer(stage.scope).await?;
            let signed = rocky_credentials::SignedMigrationPublication::endorse(
                statement,
                rocky_credentials::Signer::node(&signer.node, &signer.credentials)?,
                &signer.links,
                writer,
            )?;
            self.replica.admit_migration_publication(signed).await?;
        }
        self.replica
            .release_committed_schema(stage.publication)
            .await?;
        self.finalize_seals().await;
        Ok(())
    }

    async fn withdraw_stage(&self, stage: &SchemaStage) -> Result<(), Failure> {
        let manifest = self
            .replica
            .tip_graph(stage.scope)
            .await?
            .schema(stage.source)?;
        let bytes = self
            .replica
            .tip_schema(manifest)
            .await?
            .ok_or("the source tip's schema record is missing")?;
        let record = Record::parse(&bytes)?;
        let mut settings = self.settings.lock().await;
        let domain = record.domain(&settings, manifest)?;
        let mut draft = (**settings).clone();
        draft.pending = Some(crate::tip_schemas::Pending {
            tip: stage.source,
            manifest,
        });
        self.replica
            .put_authority_state(
                crate::enrollment::AUTHORITY_IDENTITY_KEY,
                &draft.to_bytes()?,
                now()?,
            )
            .await?;
        draft.schema = record.schema;
        draft.generation = record.generation;
        draft.engine = record.engine;
        draft.prepare_schema(&self.vault, &domain)?;
        self.replica
            .withdraw_staged_schema(
                stage.publication,
                domain.app.clone(),
                rocky_secure::SchemaConfig {
                    security: draft.config(&self.vault, &domain)?,
                    tables: Some((domain.logical().clone(), domain.scope, domain.readers())),
                },
            )
            .await?;
        crate::database::attach(&self.replica, &mut draft, &self.vault, &domain).await?;
        crate::database::retain(&self.replica, &draft, &domain).await?;
        draft.pending = None;
        settings.commit(Shared::new(draft), &self.replica).await?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = settings.granted(&domain);
        *self.domain.write().map_err(|_| "domain catalog poisoned")? = Arc::new(domain);
        drop(settings);
        self.try_ensure_tip_node().await;
        Ok(())
    }
}

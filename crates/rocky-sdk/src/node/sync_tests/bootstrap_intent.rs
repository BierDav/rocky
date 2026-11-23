use super::*;
use crate::{
    bootstrap_intent::Imports,
    pause::{self, Point},
};
use rocky_credentials::{Revocation, RevocationMode, SignedRevocation, Signer};

async fn revocation_by(issuer: &Node) -> Result<SignedRevocation, Failure> {
    let key = Settings::node_key(&issuer.vault)?;
    let settings = issuer.settings.lock().await.clone();
    let statement = Revocation {
        app: settings.domain()?.manifest().app,
        issuer: settings.identity,
        time: now()?,
        nodes: Default::default(),
        identities: [PrivateKey::new().public_key()].into(),
        capabilities: Default::default(),
        cut: Default::default(),
        mode: RevocationMode::Lost,
    };
    Ok(SignedRevocation::sign(
        statement,
        Signer::node(&key, &settings.credentials)?,
        &settings.issuer_links(),
    )?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reimport_requested_by_a_revocation_while_an_import_step_records_progress_is_kept()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let c_root = root.path().join("c");
            let c = enrolled(
                &c_root,
                &hub,
                "c",
                Some((
                    &a,
                    Profile::Writer {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            let inviter = Settings::node_key(&a.vault)?.public_key();
            let scope = c.app_scopes()[0];
            let totals = [sync::Planned {
                scope,
                checkpoint: [7; 32],
                rows: 3,
            }];
            c.bootstrap_intent.join(inviter).await?;
            let joined = c
                .bootstrap_intent
                .load()
                .await?
                .ok_or("the join records an intent")?;
            let importing = c
                .bootstrap_intent
                .record_progress(&joined, &[scope], &totals)
                .await?;
            c.replica
                .learn_revocations(&RevocationSet::try_from(vec![revocation_by(&a).await?])?)
                .await?;

            let (loaded, release_import_step) = pause::arm(&c_root, Point::BootstrapIntentLoaded);
            let import_step = {
                let c = c.clone();
                tokio::spawn(async move {
                    c.bootstrap_intent
                        .record_progress(&importing, &[], &totals)
                        .await
                        .map_err(|error| error.to_string())
                })
            };
            loaded.await?;
            let (contended, _) = pause::arm(&c_root, Point::BootstrapIntentContended);
            let mut absorbing = {
                let c = c.clone();
                tokio::spawn(async move {
                    c.absorb_revocations(None)
                        .await
                        .map_err(|error| error.to_string())
                })
            };
            let absorbed_inside_the_window = tokio::select! {
                biased;
                _ = contended => None,
                absorbed = &mut absorbing => Some(absorbed),
            };
            let _ = release_import_step.send(());
            import_step.await??;
            match absorbed_inside_the_window {
                Some(absorbed) => absorbed?,
                None => absorbing.await?,
            }?;

            let intent = c
                .bootstrap_intent
                .load()
                .await?
                .ok_or("the requested re-import keeps an intent")?;
            assert_eq!(
                intent.imports(),
                &Imports::Granted,
                "a re-import requested while an import step held the loaded intent must not be overwritten by its progress"
            );
            assert!(intent.totals().is_empty());
            assert_eq!(intent.inviter(), Some(inviter));
            for node in [&a, &c] {
                node.detach(None).await;
            }
            Ok(())
        })
        .await
}

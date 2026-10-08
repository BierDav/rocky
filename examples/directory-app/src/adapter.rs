use crate::*;
impl Application for DirectoryApp {
    type State = ();
    type Error = Invalid;
    type Policy = Policy;
    fn validate(&self, _: &Envelope) -> Result<(), Invalid> {
        Err(Invalid)
    }
    fn admit(&self, _: &Envelope, _: &Policy) -> Result<(), Invalid> {
        Err(Invalid)
    }
    fn apply_ready(&self, _: &mut (), _: &Envelope, _: AdmissionBoundary) -> Result<(), Invalid> {
        Err(Invalid)
    }
}
impl CompiledSchema for DirectoryApp {
    fn schema(&self) -> &'static [&'static str] {
        &["CREATE TABLE directory(id INTEGER PRIMARY KEY,body BLOB NOT NULL)"]
    }
    fn projection_query(&self) -> &'static str {
        "SELECT body FROM directory WHERE id=0"
    }
    fn checkpoint_tables(&self) -> &'static [&'static str] {
        // The mergeable state is empty; `directory` holds command results.
        &[]
    }
}
impl StoredApplication for DirectoryApp {
    type Row = Directory;
    fn descriptor(&self) -> ApplicationDescriptor {
        ApplicationDescriptor {
            definition: "directory.v1",
            operations: Versions::V1,
            projection: 1,
        }
    }
    fn manifest(&self) -> Option<ApplicationManifest> {
        Some(DirectoryApp::manifest(self))
    }
    fn commands(&self) -> &'static [CommandMethod] {
        &[CommandMethod {
            name: COMMAND,
            version: 1,
            schema: 1,
            handler: 1,
        }]
    }
    fn command(&self, method: &str) -> Option<&dyn CommandHandler<Self>> {
        (method == COMMAND).then_some(self)
    }
    fn projection_rows(&self, _: &()) -> Vec<Vec<SqlValue>> {
        vec![]
    }
    fn touched(&self, _: &Envelope) -> Vec<ProjectionStatement> {
        vec![]
    }
    fn window(&self, _: Vec<Vec<SqlValue>>) -> Result<(), Invalid> {
        Ok(())
    }
    fn project(&self, _: &(), _: &Envelope) -> Vec<ProjectionStatement> {
        vec![]
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Directory>, Invalid> {
        rows.into_iter()
            .map(|row| match row.as_slice() {
                [SqlValue::Blob(data)] => Directory::decode(data),
                _ => Err(Invalid),
            })
            .collect()
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &Policy,
    ) -> Result<(), Invalid> {
        if scope == policy.scope
            && (caller == policy.caller || policy.permitted_callers.contains(&caller))
        {
            Ok(())
        } else {
            Err(Invalid)
        }
    }
    fn read_views(
        &self,
        context: AdmissionContext,
        policy: &Policy,
    ) -> Result<Vec<ReadView>, Invalid> {
        self.admit_read(context.scope, context.caller, policy)?;
        Ok(vec![ReadView {
            name: "directory".into(),
            sql: "SELECT body FROM directory WHERE id=1".into(),
            values: Vec::new(),
            // One row: nothing to page.
            ..ReadView::default()
        }])
    }
}
impl CommandHandler<DirectoryApp> for DirectoryApp {
    fn seal(&self, _: &()) -> Result<Vec<u8>, Invalid> {
        Ok(vec![])
    }
    fn admit(&self, request: &StrongRequest, policy: &Policy) -> Result<(), Invalid> {
        self.admit_read(request.scope, request.caller, policy)?;
        if request.policy != policy.reference {
            return Err(Invalid);
        }
        let transition: Transition =
            serde_json::from_slice(&request.arguments).map_err(|_| Invalid)?;
        let time = match transition {
            Transition::RegisterDeployment { time, .. }
            | Transition::RegisterNode { time, .. }
            | Transition::Delegate { time, .. } => Some(time),
            _ => None,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Invalid)?
            .as_secs();
        if time.is_some_and(|time| time.abs_diff(now) > 30) {
            return Err(Invalid);
        }
        Ok(())
    }
    fn select_result(
        &self,
        data: &[u8],
        context: AdmissionContext,
        policy: &Policy,
    ) -> Result<bool, Invalid> {
        self.admit_read(context.scope, context.caller, policy)?;
        Directory::decode(data)?;
        Ok(true)
    }
    fn validate(&self, request: &StrongRequest) -> Result<(), Invalid> {
        if request.method != COMMAND
            || request.version != 1
            || request.schema != 1
            || request.handler != 1
            || request.arguments.len() > 16 * 1024
        {
            return Err(Invalid);
        }
        let _: Transition = serde_json::from_slice(&request.arguments).map_err(|_| Invalid)?;
        Ok(())
    }
    fn execute(&self, state: &(), request: &StrongRequest) -> Result<Vec<u8>, Invalid> {
        self.execute_after(state, request, None)
    }
    fn execute_after(
        &self,
        _: &(),
        request: &StrongRequest,
        previous: Option<&[u8]>,
    ) -> Result<Vec<u8>, Invalid> {
        <Self as CommandHandler<Self>>::validate(self, request)?;
        let directory = previous
            .map(Directory::decode)
            .transpose()?
            .unwrap_or(Directory {
                ownership: self.initial_ownership.clone(),
                ..Directory::default()
            });
        self.transition(
            directory,
            request.caller,
            serde_json::from_slice(&request.arguments).map_err(|_| Invalid)?,
        )?
        .encode()
    }
    fn result_projection_query(&self) -> Option<&'static str> {
        Some("SELECT body FROM directory WHERE id=1")
    }
    fn result_projection_rows(&self, data: &[u8]) -> Result<Vec<Vec<SqlValue>>, Invalid> {
        Directory::decode(data)?;
        Ok(vec![vec![SqlValue::Blob(data.to_vec())]])
    }
    fn project_result(&self, data: &[u8]) -> Result<Vec<ProjectionStatement>, Invalid> {
        Directory::decode(data)?;
        Ok(vec![ProjectionStatement {
            sql:
                "INSERT INTO directory VALUES(1,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body"
                    .into(),
            values: vec![SqlValue::Blob(data.to_vec())],
        }])
    }
}

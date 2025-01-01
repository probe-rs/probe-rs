use crate::cmd::remote::{functions::RemoteFunctions, LocalSession, SessionId};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ResumeAllCores {
    pub sessid: SessionId,
}

impl super::RemoteFunction for ResumeAllCores {
    type Result = ();

    async fn run(self, iface: &mut LocalSession) -> Self::Result {
        iface.session(self.sessid).resume_all_cores().unwrap();
    }
}

impl From<ResumeAllCores> for RemoteFunctions {
    fn from(func: ResumeAllCores) -> Self {
        RemoteFunctions::ResumeAllCores(func)
    }
}

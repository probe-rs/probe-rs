use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::{Key, Session};

#[derive(Serialize, Deserialize, Schema)]
pub struct ResumeAllCoresRequest {
    pub sessid: Key<Session>,
}

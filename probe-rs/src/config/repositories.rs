#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Repositories {
    #[serde(flatten)]
    repositories: Vec<Repository>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub url: String,
}
#[derive(Debug, Default)]
pub struct Type {
    pub name: String,
    pub named_children: Option<std::collections::HashMap<String, Type>>,
    pub indexed_children: Option<Vec<Type>>,
}

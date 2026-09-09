#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub display_order: i64,
    pub title: String,
    pub status: String,
    pub due: Option<String>,
    pub log: String,
    pub created_at: String,
    pub updated_at: String,
}

pub struct NumaInfo {
    node_count: usize,
    current_node: NodeId,
}

impl NumaInfo {
    pub fn detect() -> Option<Self> {
        if !numa::is_available() {
            return None;
        }
        Some(Self {
            node_count: numa::nodes().count(),
            current_node: numa::current_node(),
        })
    }
}
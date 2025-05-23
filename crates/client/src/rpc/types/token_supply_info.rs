use mazze_types::U256;

#[derive(Debug, Serialize, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenSupplyInfo {
    pub total_circulating: U256,
    pub total_issued: U256,
    pub total_collateral: U256,
    pub total_espace_tokens: U256,
}

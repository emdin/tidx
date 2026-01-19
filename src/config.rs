pub struct Config {
    pub rpc_url: String,
    pub database_url: String,
}

pub const PRESTO_CHAIN_ID: u64 = 4217;
pub const ANDANTINO_CHAIN_ID: u64 = 42429;
pub const MODERATO_CHAIN_ID: u64 = 42431;

pub fn chain_name(chain_id: u64) -> &'static str {
    match chain_id {
        PRESTO_CHAIN_ID => "Presto",
        ANDANTINO_CHAIN_ID => "Andantino",
        MODERATO_CHAIN_ID => "Moderato",
        _ => "Unknown",
    }
}

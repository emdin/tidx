use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::Args as ClapArgs;

use crate::db;

#[derive(ClapArgs)]
pub struct Args {
    /// Database URL
    #[arg(long, env = "AK47_DATABASE_URL")]
    pub db: String,

    /// Watch mode - continuously update status
    #[arg(long, short)]
    pub watch: bool,
}

pub async fn run(args: Args) -> Result<()> {
    let pool = db::create_pool(&args.db).await?;

    loop {
        let conn = pool.get().await?;

        let state = conn
            .query_opt(
                "SELECT chain_id, head_num, synced_num, updated_at FROM sync_state WHERE id = 1",
                &[],
            )
            .await?;

        if args.watch {
            print!("\x1B[2J\x1B[1;1H");
        }

        println!("AK47 Status");
        println!("═══════════════════════════════════════");

        match state {
            Some(row) => {
                let chain_id: i64 = row.get(0);
                let head: i64 = row.get(1);
                let synced: i64 = row.get(2);
                let updated: DateTime<Utc> = row.get(3);

                let chain_name = match chain_id {
                    4217 => "Presto",
                    42429 => "Andantino",
                    42431 => "Moderato",
                    _ => "Unknown",
                };

                let lag = head - synced;
                let age = Utc::now().signed_duration_since(updated);

                println!("Network:    {} ({})", chain_name, chain_id);
                println!("Head:       {}", head);
                println!("Synced:     {}", synced);
                println!("Lag:        {} blocks", lag);
                println!(
                    "Updated:    {} ({} ago)",
                    updated.format("%H:%M:%S"),
                    format_duration(age)
                );
            }
            None => {
                println!("No sync state found. Run 'ak47 up' to start syncing.");
            }
        }

        if !args.watch {
            break;
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    Ok(())
}

fn format_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

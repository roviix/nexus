use nexus_accounts::AccountsService;
use nexus_cursor::Cursor;
use nexus_store::{Db, SqliteSecrets};
use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    let data_dir = PathBuf::from(std::env::var("HOME").unwrap())
        .join("Library/Application Support/com.roviix.nexus");

    println!("Using data dir: {:?}", data_dir);
    let db = Arc::new(Db::open(data_dir.join("nexus.db")).expect("open db"));
    let cursor = Cursor::detect().expect("detect cursor");
    let secrets = Arc::new(SqliteSecrets::new(db.clone()));
    let accounts = Arc::new(AccountsService::new(db.clone(), secrets));

    let crsr = nexus_crsr::CrsrService::new(
        db.clone(),
        &data_dir,
        cursor.paths.clone(),
        Arc::new(cursor.control()),
        accounts,
    );

    println!("Current CRSR status: {:?}", crsr.status());
    println!("Applying install...");
    match crsr.install(false, &|p| println!("Progress: {:?}", p)) {
        Ok(outcome) => {
            println!("Installed successfully! Outcome: {:?}", outcome);
        }
        Err(e) => {
            eprintln!("Install failed: {:?}", e);
        }
    }
}

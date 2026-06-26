//! `mod_db_lookup` — showcases the built-in SQLite wrapper ([`fswtch::CoreDb`] + [`fswtch::Stmt`]).
//!
//! FreeSWITCH ships SQLite (`switch_core_db_*`); this module wraps it with small-`sqlite3-rs`
//! ergonomics. The `rust_db_lookup` API command opens a private in-memory database, creates a
//! table, prepares an `INSERT` with a bound `?` text parameter, steps it, then prepares a
//! `SELECT`, steps once, and reads the round-tripped value back out — writing the result to the
//! stream. Unlike the media/codec examples this needs only the FreeSWITCH core (always present),
//! not a live call session.
//!
//! Build as `mod_db_lookup.so`, load with `load mod_db_lookup;`, then from `fs_cli`:
//!   `rust_db_lookup`           — runs the round-trip and prints the stored value
//!   `rust_db_lookup <text>`     — stores `<text>` instead of the default "fswtch"

fswtch::module_exports! {
    module = mod_db_lookup,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn db_lookup_api(cmd, _session, stream) {
        fswtch::log_info("mod_db_lookup", "rust_db_lookup invoked");

        let outcome: Result<String, String> = (|| {
            let cmd = cmd.unwrap_or_default();
            let value = if cmd.trim().is_empty() {
                "fswtch".to_owned()
            } else {
                cmd.trim().to_owned()
            };

            let db = fswtch::CoreDb::open(":memory:")
                .map_err(|e| format!("open failed: {e}"))?;

            db.exec("CREATE TABLE kv(id INTEGER PRIMARY KEY, val TEXT)")
                .map_err(|e| format!("create failed: {e}"))?;

            {
                let mut ins = db
                    .prepare("INSERT INTO kv(val) VALUES (?)")
                    .map_err(|e| format!("prepare insert failed: {e}"))?;
                ins.bind_text(1, &value)
                    .map_err(|e| format!("bind_text failed: {e}"))?;
                let more = ins.step().map_err(|e| format!("step insert failed: {e}"))?;
                if more {
                    return Err("unexpected row from INSERT".to_owned());
                }
            }

            let sel = db
                .prepare("SELECT id, val FROM kv")
                .map_err(|e| format!("prepare select failed: {e}"))?;
            let cols = sel.column_count();
            let have_row = sel.step().map_err(|e| format!("step select failed: {e}"))?;
            if !have_row {
                return Err("SELECT returned no rows".to_owned());
            }
            let id = sel
                .column_text(0)
                .ok_or_else(|| "id column was NULL".to_owned())?;
            let read_back = sel
                .column_text(1)
                .ok_or_else(|| "val column was NULL".to_owned())?;

            Ok(format!(
                "rows_stored={} cols={} id={} val={}\n",
                db.changes(),
                cols,
                id,
                read_back
            ))
        })();

        match outcome {
            Ok(line) => stream.write(&line),
            Err(line) => stream.write(&format!("error: {line}\n")),
        }
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_db_lookup" {
        fswtch::log_info("mod_db_lookup", "loading module");
        module.api(
            "rust_db_lookup",
            "round-trips a value through the in-memory SQLite (CoreDb) wrapper",
            "rust_db_lookup [value]",
            db_lookup_api,
        )
    }
}

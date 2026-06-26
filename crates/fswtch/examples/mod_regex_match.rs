//! Showcase: PCRE2 regex matching via `fswtch::Regex` and `fswtch::RegexMatch`.
//!
//! Registers the `rust_regex_match` API command. From `fs_cli` run:
//!   `rust_regex_match <pattern> <subject>`
//! e.g. `rust_regex_match ^(\w+)-(\w+)$ foo-bar` compiles the pattern with
//! `Regex::compile`, matches the subject with `Regex::matches`, and writes the
//! whole match (capture group 0) plus any named groups to the stream.

fswtch::module_exports! {
    module = mod_regex_match,
    load = switch_module_load,
}

fswtch::api_callback! {
    fn regex_match_api(cmd, _session, stream) {
        fswtch::log_info("mod_regex_match", "rust_regex_match invoked");

        // `cmd` is the full API command line (everything after the command name).
        // Treat it as "<pattern> <subject>", splitting on the first space.
        let cmd = match cmd {
            Some(cmd) => cmd,
            None => {
                return stream.write("usage: rust_regex_match <pattern> <subject>\n");
            }
        };

        let (pattern, subject) = match cmd.split_once(' ') {
            Some((p, s)) => (p.trim(), s.trim_start_matches(' ')),
            None => {
                return stream.write(
                    "usage: rust_regex_match <pattern> <subject>\n",
                );
            }
        };

        if pattern.is_empty() || subject.is_empty() {
            return stream.write(
                "usage: rust_regex_match <pattern> <subject>\n",
            );
        }

        // Compile the pattern once (options = 0 = case-sensitive, PCRE2 defaults).
        // A second `.api(...)` is chained in `module_load!` to demonstrate reuse of
        // the same compiled regex across both a boolean test and a capture test.
        let regex = match fswtch::Regex::compile(pattern, 0) {
            Ok(regex) => regex,
            Err(error) => {
                return stream.write(&format!("compile error: {error}\n"));
            }
        };

        match regex.matches(subject) {
            Ok(Some(m)) => {
                // Whole match is capture group 0.
                let whole = m
                    .capture(0)
                    .ok()
                    .flatten()
                    .unwrap_or_default();

                // Walk the capture groups and append each to the output.
                let mut out = format!("match: {whole}\n");
                let groups = m.group_count();
                for i in 1..groups {
                    if let Ok(Some(g)) = m.capture(i) {
                        out.push_str(&format!("  group {i}: {g}\n"));
                    }
                }
                stream.write(&out)
            }
            Ok(None) => stream.write("no match\n"),
            Err(error) => stream.write(&format!("match error: {error}\n")),
        }
    }
}

fswtch::api_callback! {
    fn regex_test_api(cmd, _session, stream) {
        fswtch::log_info("mod_regex_match", "rust_regex_test invoked");

        // A boolean-only convenience: `rust_regex_test <pattern> <subject>`
        // reports match / no-match via `Regex::is_match`.
        let cmd = match cmd {
            Some(cmd) => cmd,
            None => {
                return stream.write("usage: rust_regex_test <pattern> <subject>\n");
            }
        };

        let (pattern, subject) = match cmd.split_once(' ') {
            Some((p, s)) => (p.trim(), s.trim_start_matches(' ')),
            None => {
                return stream.write(
                    "usage: rust_regex_test <pattern> <subject>\n",
                );
            }
        };

        if pattern.is_empty() || subject.is_empty() {
            return stream.write(
                "usage: rust_regex_test <pattern> <subject>\n",
            );
        }

        match fswtch::Regex::compile(pattern, 0) {
            Ok(regex) => {
                if regex.is_match(subject) {
                    stream.write("match: true\n")
                } else {
                    stream.write("match: false\n")
                }
            }
            Err(error) => stream.write(&format!("compile error: {error}\n")),
        }
    }
}

fswtch::module_load! {
    fn switch_module_load(module) for "mod_regex_match" {
        fswtch::log_info("mod_regex_match", "loading module");
        module
            .api(
                "rust_regex_match",
                "compiles a PCRE2 pattern and writes the whole match + capture groups",
                "rust_regex_match <pattern> <subject>",
                regex_match_api,
            )
            .and_then(|module| {
                module.api(
                    "rust_regex_test",
                    "compiles a PCRE2 pattern and reports a boolean match result",
                    "rust_regex_test <pattern> <subject>",
                    regex_test_api,
                )
            })
    }
}

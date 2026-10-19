//! The exports a plugin installer reads are exactly their decoder's dispatch
//! arms, so the plugin forwards every event the decoder handles.

#[cfg(test)]
mod tests {
    /// Every dispatch arm in `file` names a CONST, and those consts are exactly
    /// the ones `export` declares.
    ///
    /// An arm written as a bare literal decodes a name no export carries, so a
    /// bare literal is rejected outright.
    fn assert_arms_match_export(file: &str, dispatch: &str, export: &str) {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/source")
                .join(file),
        )
        .expect("the module is readable");

        let after = src
            .split_once(dispatch)
            .unwrap_or_else(|| panic!("{file}: no `{dispatch}` dispatch"))
            .1;
        let body = &after[..after.find("\n}\n").unwrap_or(after.len())];
        // The bare-literal ban runs at EVERY depth, not just the level the export
        // is collected from: a nested `match` would otherwise hide wire values
        // behind an indent the collector skips.
        for line in body.lines() {
            let Some((head, _)) = line.split_once("=>") else {
                continue;
            };
            // ANY `"` in the head, not just a leading or `(`-prefixed one: the
            // shape that escaped was a guard, `g if g == "wire_name" =>`, which
            // dispatches on a name no export carries while `armed` stays
            // equal to the export because the token is not SCREAMING_SNAKE.
            assert!(
                !head.contains('"'),
                "{file}: arm `{}` in {export}'s dispatch reads a bare wire literal — \
                 name it as a const",
                head.trim(),
            );
        }
        let mut armed: Vec<&str> = Vec::new();
        for line in body.lines() {
            if !line.contains("=>")
                || !line.starts_with("        ")
                || line.starts_with("         ")
            {
                continue;
            }
            for tok in line
                .split("=>")
                .next()
                .unwrap_or("")
                .split('|')
                .map(str::trim)
            {
                if tok != "_"
                    && !tok.is_empty()
                    && tok
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
                {
                    armed.push(tok);
                }
            }
        }

        let decl = src
            .split_once(&format!("{export}: &[&str] = &["))
            .unwrap_or_else(|| panic!("{file}: no {export} declaration"))
            .1;
        let mut declared: Vec<&str> = decl[..decl.find("];").unwrap_or(0)]
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();

        armed.sort_unstable();
        armed.dedup();
        declared.sort_unstable();
        assert_eq!(armed, declared, "{file}: dispatch arms vs {export}");
    }

    #[test]
    fn every_exported_set_is_exactly_its_dispatch_arms() {
        let rows = [
            ("opencode.rs", "match event {", "DECODED_EVENTS"),
            ("omp.rs", "match ty {", "DECODED_HOOK_EVENTS"),
        ];
        for (file, dispatch, export) in rows {
            assert_arms_match_export(file, dispatch, export);
        }
    }
}

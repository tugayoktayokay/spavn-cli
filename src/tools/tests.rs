pub(super) use super::*;
pub(super) use serde_json::json;

#[cfg(test)]
mod phase_tests {
    use super::*;
    use crate::spavn_engine::AgentPhase;

    fn ctx(phase: AgentPhase) -> ToolContext {
        ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            PathBuf::from("/tmp"),
            phase,
        )
    }

    #[test]
    fn plan_locks_write_edit_bash() {
        let mut c = ctx(AgentPhase::Plan);
        for tool in ["write", "edit", "bash"] {
            let out = execute_tool(&mut c, tool, "{}");
            assert!(
                out.contains("READ-ONLY PHASE"),
                "tool {tool} should be locked in PLAN, got: {out}"
            );
        }
    }

    #[test]
    fn code_allows_read() {
        let mut c = ctx(AgentPhase::Code);
        let out = execute_tool(&mut c, "read", r#"{"path":"/nonexistent-phase-test-xyz"}"#);
        assert!(!out.contains("READ-ONLY PHASE"), "read in CODE: {out}");
    }

    #[test]
    fn test_binaries_always_report_a_non_tty_stdin() {
        // Approval prompts fail closed on a non-TTY stdin. `cargo test` inherits the developer's
        // terminal, so this used to answer "TTY" and the suite blocked on the prompt forever.
        // Pinning it here keeps a future refactor of `stdin_is_tty` from reintroducing the hang.
        assert!(
            !crate::hud::stdin_is_tty(),
            "test builds must never see a tty"
        );
    }

    #[test]
    fn plan_allows_read_and_memory() {
        // Guards the fail-closed approval path: with no TTY the prompt answers "no", so any
        // read that needs approval would silently become a block in every test below.
        assert!(!crate::hud::stdin_is_tty(), "tests run without a tty");
        let mut c = ctx(AgentPhase::Plan);
        let out = execute_tool(&mut c, "memory", r#"{"op":"recall","query":"x"}"#);
        assert!(!out.contains("READ-ONLY PHASE"), "memory in PLAN: {out}");
        let out = execute_tool(&mut c, "read", r#"{"path":"/nonexistent-phase-test-xyz"}"#);
        assert!(!out.contains("READ-ONLY PHASE"), "read in PLAN: {out}");
    }

    #[test]
    fn batch_keeps_declared_order_and_distinct_steps() {
        let mut c = ctx(AgentPhase::Code);
        let calls = vec![
            (
                "glob".to_string(),
                r#"{"pattern":"Cargo.toml"}"#.to_string(),
            ),
            (
                "read".to_string(),
                r#"{"path":"/nonexistent-batch-a"}"#.to_string(),
            ),
            (
                "read".to_string(),
                r#"{"path":"/nonexistent-batch-b"}"#.to_string(),
            ),
        ];
        let out = super::execute_read_only_batch(&mut c, &calls);
        assert_eq!(out.len(), 3, "one result per call: {out:?}");
        assert!(out[1].starts_with("Read error"), "{}", out[1]);
        assert!(out[2].starts_with("Read error"), "{}", out[2]);
        assert!(!out[0].contains("Read error"), "{}", out[0]);
        // Three calls consumed three steps, even though they ran as one turn.
        assert_eq!(c.tool_step_counter, 3);
    }

    #[test]
    fn batch_refuses_a_single_call() {
        let mut c = ctx(AgentPhase::Code);
        let calls = vec![("read".to_string(), r#"{"path":"/x"}"#.to_string())];
        assert!(super::execute_read_only_batch(&mut c, &calls).is_empty());
    }

    #[test]
    fn a_non_sovereign_context_never_batches() {
        let mut c = ToolContext::new(
            AccessMode::Safe,
            "local".to_string(),
            PathBuf::from("/tmp"),
            AgentPhase::Code,
        );
        let calls = vec![
            (
                "read".to_string(),
                r#"{"path":"/nonexistent-a"}"#.to_string(),
            ),
            (
                "read".to_string(),
                r#"{"path":"/nonexistent-b"}"#.to_string(),
            ),
        ];
        assert!(super::execute_read_only_batch(&mut c, &calls).is_empty());
        assert_eq!(c.tool_step_counter, 0, "a refused batch consumes no steps");
    }

    /// A batched read must leave the same per-file state a serial read would, otherwise the
    /// very next `edit` is accused of shifting code the model did read.
    #[test]
    fn batched_read_satisfies_the_code_shifting_guard() {
        let dir = std::env::temp_dir().join(format!("spavn-batch-guard-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("a.txt");
        fs::write(&target, "one\ntwo\n").unwrap();
        let other = dir.join("b.txt");
        fs::write(&other, "alpha\nbeta\n").unwrap();

        let mut c = ctx(AgentPhase::Code);
        let calls = vec![
            (
                "read".to_string(),
                format!(
                    r#"{{"path":"{}","start_line":1,"end_line":2}}"#,
                    target.display()
                ),
            ),
            (
                "read".to_string(),
                format!(
                    r#"{{"path":"{}","start_line":1,"end_line":2}}"#,
                    other.display()
                ),
            ),
        ];
        let out = super::execute_read_only_batch(&mut c, &calls);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(c
            .read_mtimes
            .contains_key(&target.to_string_lossy().to_string()));

        let edited = execute_tool(
            &mut c,
            "edit",
            &format!(
                r#"{{"path":"{}","old_text":"one","new_text":"uno"}}"#,
                target.display()
            ),
        );
        assert!(
            !edited.contains("CODE-SHIFTING GUARD"),
            "batched read should satisfy the guard: {edited}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mutating_tools_never_join_a_batch() {
        for name in ["edit", "write", "bash", "subagent", "lsp", "memory"] {
            assert!(!super::parallel_safe_tool(name), "{name} must stay serial");
        }
        for name in ["read", "search", "glob", "skill", "web"] {
            assert!(super::parallel_safe_tool(name), "{name} should batch");
        }
    }

    #[test]
    fn phase_from_str_and_toggle() {
        assert_eq!(AgentPhase::parse_alias("plan"), Some(AgentPhase::Plan));
        assert_eq!(AgentPhase::parse_alias("CODE"), Some(AgentPhase::Code));
        assert_eq!(AgentPhase::parse_alias("bogus"), None);
        assert_eq!(AgentPhase::Plan.toggled(), AgentPhase::Code);
        assert_eq!(AgentPhase::Code.toggled(), AgentPhase::Plan);
    }

    #[test]
    fn plan_schema_omits_mutating_tools() {
        let plan_tools = super::tool_definitions_for_phase(AgentPhase::Plan);
        let arr = plan_tools.as_array().expect("array");
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|v| v.get("function")?.get("name")?.as_str())
            .collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"search"));
        assert!(names.contains(&"subagent"));
        assert!(names.contains(&"lsp"));
        assert!(!names.contains(&"edit"));
        assert!(!names.contains(&"write"));
        assert!(!names.contains(&"bash"));
    }

    #[test]
    fn subagent_rejects_empty_task_gracefully() {
        let mut c = ctx(AgentPhase::Plan);
        let out = execute_tool(&mut c, "subagent", "{}");
        assert!(out.contains("missing 'task' parameter"), "{out}");
    }

    #[test]
    fn read_tool_handles_png_image_as_multimodal_vision_instead_of_utf8_error() {
        let dir = std::env::temp_dir().join(format!("spavn-vision-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let img_path = dir.join("test_render.png");
        // Non-UTF8 PNG header bytes (`\x89PNG\r\n\x1a\n`)
        fs::write(&img_path, b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR").unwrap();

        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            dir.clone(),
            AgentPhase::Code,
        );
        let args = serde_json::json!({ "path": img_path.to_string_lossy() });
        let out = execute_tool(&mut c, "read", &args.to_string());
        assert!(
            out.contains("[SPAVN Vision Image Loaded:"),
            "expected vision image load, got: {out}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reindent_replacement_preserves_file_base_indent_and_nesting() {
        let matched = ["        if cond {", "            do_a();", "        }"];
        let old_text = "if cond {\n    do_a();\n}";
        let new_text = "if cond {\n    do_b();\n}";
        let out = super::reindent_replacement_lines(&matched, old_text, new_text);
        assert_eq!(
            out,
            vec![
                "        if cond {".to_string(),
                "            do_b();".to_string(),
                "        }".to_string()
            ]
        );
    }
}

#[cfg(all(test, unix))]
mod timeout_tests {
    use super::*;

    #[test]
    fn timeout_kills_grandchildren_and_does_not_hang() {
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 30 & sleep 30"]);
        let t = std::time::Instant::now();
        let (_, _, _, timed_out) = run_command_with_timeout(c, 1).unwrap();
        assert!(timed_out && t.elapsed().as_secs() < 5);
    }

    #[test]
    fn signal_exit_is_nonzero() {
        let mut c = Command::new("sh");
        c.args(["-c", "kill -9 $$"]);
        assert_eq!(run_command_with_timeout(c, 5).unwrap().0, 137);
    }

    #[test]
    fn interactive_subshell_is_detached_from_controlling_tty() {
        let mut c = Command::new("sh");
        c.args(["-i", "-c", "exit 0"]);
        let (code, _, _, timed_out) = run_command_with_timeout(c, 5).unwrap();
        assert!(!timed_out);
        assert_eq!(code, 0);
    }

    #[test]
    fn background_job_lifecycle_status_list_and_kill() {
        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            std::env::temp_dir(),
            crate::spavn_engine::AgentPhase::Code,
        );
        let start_out = exec_bash(
            &mut c,
            &json!({ "command": "echo READY_BG && sleep 30", "background": true }),
        );
        assert!(start_out.contains("bg_"), "start output: {start_out}");
        let id_start = start_out.find("bg_").unwrap();
        let job_id: String = start_out[id_start..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        std::thread::sleep(std::time::Duration::from_millis(120));

        let list_out = exec_bash(&mut c, &json!({ "op": "list" }));
        assert!(list_out.contains(&job_id), "list output: {list_out}");

        let status_out = exec_bash(&mut c, &json!({ "op": "status", "job_id": job_id }));
        assert!(
            status_out.contains("READY_BG"),
            "status output: {status_out}"
        );
        assert!(
            status_out.contains("running"),
            "status output: {status_out}"
        );

        let kill_out = exec_bash(&mut c, &json!({ "op": "kill", "job_id": job_id }));
        assert!(
            kill_out.contains("Killed background job"),
            "kill output: {kill_out}"
        );
    }
}

#[cfg(all(test, unix))]
#[cfg(all(test, unix))]
mod bash_spill_tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            std::env::temp_dir(),
            crate::spavn_engine::AgentPhase::Code,
        )
    }

    #[test]
    fn oversized_output_keeps_head_and_tail_and_points_at_the_spill_file() {
        let out = exec_bash(&mut ctx(), &json!({ "command": "seq 1 6000" }));
        assert!(out.contains("SPAVN truncated"), "no marker: {out}");
        assert!(
            out.starts_with("1\n"),
            "head lost: {}",
            &out[..40.min(out.len())]
        );
        assert!(out.trim_end().ends_with("6000"), "tail lost");
        let path = out
            .split('`')
            .find(|seg| seg.contains("spavn") && seg.ends_with(".txt"))
            .unwrap_or_else(|| panic!("no spill path in marker: {out}"));
        let full = fs::read_to_string(path).expect("spill file should exist and be readable");
        assert!(
            full.contains("\n3000\n"),
            "middle should survive in the spill file"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn small_output_is_returned_untouched_and_spills_nothing() {
        let out = exec_bash(&mut ctx(), &json!({ "command": "echo hello-spill-probe" }));
        assert_eq!(out.trim(), "hello-spill-probe");
        assert!(!out.contains("truncated"));
    }
}

#[cfg(test)]
mod subagent_header_tests {
    use super::*;

    #[test]
    fn short_tasks_are_passed_through_and_flattened() {
        assert_eq!(
            one_line_task("  find   the\nmain entry "),
            "find the main entry"
        );
    }

    #[test]
    fn long_tasks_are_cut_to_one_header_line() {
        let long = "investigate ".repeat(40);
        let label = one_line_task(&long);
        assert!(label.chars().count() <= 73, "too long: {}", label.len());
        assert!(label.ends_with('…'));
        assert!(!label.contains('\n'));
    }

    #[test]
    fn multibyte_tasks_never_split_a_char() {
        let label = one_line_task(&"ü".repeat(200));
        assert_eq!(label.chars().count(), 73);
    }
}

#[cfg(test)]
mod line_span_tests {
    use super::*;

    #[test]
    fn parses_single_bare_and_range_forms() {
        assert_eq!(parse_line_spans("42"), Some(vec![(42, 42)]));
        assert_eq!(parse_line_spans("10-20"), Some(vec![(10, 20)]));
        assert_eq!(parse_line_spans("L10-L20"), Some(vec![(10, 20)]));
        assert_eq!(parse_line_spans("10-L20"), Some(vec![(10, 20)]));
    }

    #[test]
    fn parses_comma_separated_lists() {
        assert_eq!(
            parse_line_spans("1-12,640-665"),
            Some(vec![(1, 12), (640, 665)])
        );
        assert_eq!(parse_line_spans("5,90"), Some(vec![(5, 5), (90, 90)]));
    }

    #[test]
    fn rejects_non_numeric_suffixes() {
        assert_eq!(parse_line_spans("rs"), None);
        assert_eq!(parse_line_spans(""), None);
        assert_eq!(parse_line_spans("1-"), None);
        assert_eq!(parse_line_spans("1,,2"), None);
    }

    #[test]
    fn bare_single_expands_but_multi_list_does_not() {
        assert_eq!(expand_line_spans(vec![(30, 30)]), vec![(30, 150)]);
        assert_eq!(
            expand_line_spans(vec![(5, 5), (90, 90)]),
            vec![(5, 5), (90, 90)]
        );
    }

    #[test]
    fn overlapping_and_touching_spans_merge() {
        assert_eq!(
            expand_line_spans(vec![(10, 20), (20, 30)]),
            vec![(10, 30)],
            "overlap must collapse"
        );
        assert_eq!(
            expand_line_spans(vec![(10, 20), (21, 30)]),
            vec![(10, 30)],
            "adjacent spans read as one block"
        );
        assert_eq!(
            expand_line_spans(vec![(40, 50), (1, 5)]),
            vec![(1, 5), (40, 50)],
            "output is ordered"
        );
    }

    #[test]
    fn selector_splits_a_real_multi_range_path() {
        let (path, spans) = split_inline_line_spans("src/hud/menus.rs:103-118,1060-1070");
        assert_eq!(path, "src/hud/menus.rs");
        assert_eq!(spans, Some(vec![(103, 118), (1060, 1070)]));
    }

    #[test]
    fn selector_leaves_plain_paths_untouched() {
        let (path, spans) = split_inline_line_spans("src/main.rs");
        assert_eq!(path, "src/main.rs");
        assert_eq!(spans, None);
    }

    #[test]
    fn rendered_spans_carry_absolute_lines_and_jump_markers() {
        let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let out = format_spans(&body, &[(2, 4), (20, 22)]);
        assert!(out.contains("   2: line 2"), "{}", out);
        assert!(out.contains("   4: line 4"), "{}", out);
        assert!(out.contains("[jump to line 20]"), "{}", out);
        assert!(out.contains("  22: line 22"), "{}", out);
        assert!(out.contains("EOF: line 30/30"), "{}", out);
        assert!(!out.contains("line 10"), "gap lines must not leak: {}", out);
    }
}

#[cfg(all(test, unix))]
mod glob_tests {
    use super::*;

    /// `search` and `glob` shell out to `rg`. When it is absent the raw `io::Error` ("No such file
    /// or directory") reads like a bad path and sends the model chasing the wrong thing, so the
    /// missing binary has to be named.
    #[test]
    fn a_missing_binary_is_named_in_the_error() {
        let mut cmd = Command::new("spavn-definitely-not-installed");
        let err = cmd.output().unwrap_err();
        let msg = spawn_error_message(&cmd, &err);
        assert!(msg.contains("spavn-definitely-not-installed"), "{}", msg);
        assert!(msg.contains("not installed"), "{}", msg);
        assert!(msg.contains("read"), "{}", msg);
    }

    fn ctx() -> ToolContext {
        ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            PathBuf::from("/tmp"),
            crate::spavn_engine::AgentPhase::Code,
        )
    }

    /// Builds a throwaway tree: two `.rs` files, one ignored dir, one dotfile.
    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spavn-glob-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/deep")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("src/deep/lib.rs"), "").unwrap();
        fs::write(dir.join("target/junk.rs"), "").unwrap();
        fs::write(dir.join("notes.md"), "").unwrap();
        fs::write(dir.join(".hidden.rs"), "").unwrap();
        dir
    }

    #[test]
    fn matches_only_requested_extension_and_skips_ignored_dirs() {
        let dir = fixture("ext");
        let out = exec_glob(
            &mut ctx(),
            &json!({ "pattern": "**/*.rs", "path": dir.to_string_lossy() }),
        );
        assert!(out.contains("main.rs"), "{}", out);
        assert!(out.contains("lib.rs"), "{}", out);
        assert!(!out.contains("target/"), "ignored dir leaked: {}", out);
        assert!(
            !out.contains("notes.md"),
            "extension filter leaked: {}",
            out
        );
        assert!(
            !out.contains(".hidden.rs"),
            "dotfile leaked by default: {}",
            out
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hidden_flag_opts_dotfiles_back_in() {
        let dir = fixture("hidden");
        let out = exec_glob(
            &mut ctx(),
            &json!({ "pattern": "**/*.rs", "path": dir.to_string_lossy(), "hidden": true }),
        );
        assert!(out.contains(".hidden.rs"), "{}", out);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn limit_truncates_and_says_so() {
        let dir = fixture("limit");
        let out = exec_glob(
            &mut ctx(),
            &json!({ "pattern": "**/*.rs", "path": dir.to_string_lossy(), "limit": 1 }),
        );
        assert_eq!(
            out.lines().filter(|l| l.ends_with(".rs")).count(),
            1,
            "{}",
            out
        );
        assert!(out.contains("limit 1 reached"), "{}", out);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_pattern_and_empty_match_are_explained_not_empty() {
        let dir = fixture("empty");
        assert!(exec_glob(&mut ctx(), &json!({})).contains("missing `pattern`"));
        let out = exec_glob(
            &mut ctx(),
            &json!({ "pattern": "**/*.zig", "path": dir.to_string_lossy() }),
        );
        assert!(out.contains("no files match"), "{}", out);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Regression guard for the latent bug the `**/` prefix fixes: `search` shares this glob
    /// list, so an absolute path used to walk straight into `target/` before it was normalised.
    #[test]
    fn ignore_globs_are_root_independent() {
        for pat in RG_IGNORE_GLOBS {
            assert!(
                pat.starts_with("!**/"),
                "glob {} is matched relative to the search root and breaks on absolute paths",
                pat
            );
        }
    }

    #[test]
    fn limit_is_clamped_to_the_hard_cap() {
        // A huge `limit` must not translate into an unbounded read; the clamp is pure arithmetic.
        let requested = 10_000_000u64;
        let clamped = (requested as usize).clamp(1, GLOB_MAX_LIMIT);
        assert_eq!(clamped, GLOB_MAX_LIMIT);
    }
}

#[cfg(all(test, unix))]
mod atomic_write_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn keeps_mode_follows_symlink_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("spavn-atomic-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let real = dir.join("run.sh");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.join("link.sh");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomic(&link, "new").unwrap();

        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            2,
            "temp file left behind"
        );
        let fresh = dir.join("new.txt");
        write_atomic(&fresh, "x").unwrap();
        assert_eq!(fs::read_to_string(&fresh).unwrap(), "x");

        let broken_target = dir.join("broken_target.txt");
        let broken_link = dir.join("broken_link.txt");
        std::os::unix::fs::symlink("broken_target.txt", &broken_link).unwrap();
        write_atomic(&broken_link, "healed").unwrap();
        assert!(fs::symlink_metadata(&broken_link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&broken_target).unwrap(), "healed");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_bash_gate_respects_cwd_argument_for_protected_paths() {
        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            PathBuf::from("/tmp/ws"),
            crate::spavn_engine::AgentPhase::Code,
        );
        c.policy = PathPolicy::for_test("/tmp/ws", "/tmp/ws", vec!["/protected/apps"]);
        let res = check_bash_gate(&mut c, "rm -rf proj", "local", Some("/protected/apps"));
        assert!(
            res.is_err(),
            "rm -rf proj with cwd=/protected/apps must be blocked as Catastrophic"
        );
    }

    #[test]
    fn multi_hunk_edit_applies_multiple_replacements_in_one_call() {
        let dir = std::env::temp_dir().join(format!("spavn-multiedit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.rs");
        fs::write(&file, "fn a() -> i32 { 1 }\nfn b() -> i32 { 2 }\n").unwrap();

        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            dir.clone(),
            crate::spavn_engine::AgentPhase::Code,
        );
        let args = serde_json::json!({
            "path": file.to_string_lossy(),
            "edits": [
                { "old_text": "fn a() -> i32 { 1 }", "new_text": "fn a() -> i32 { 10 }" },
                { "old_text": "fn b() -> i32 { 2 }", "new_text": "fn b() -> i32 { 20 }" }
            ]
        });
        let res = execute_tool(&mut c, "edit", &args.to_string());
        assert!(res.starts_with("Successfully "), "{res}");
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "fn a() -> i32 { 10 }\nfn b() -> i32 { 20 }\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;
    use crate::spavn_engine::AgentPhase;

    fn ctx_with(access: AccessMode, ws: &str) -> ToolContext {
        let mut c = ToolContext::new(
            access,
            "local".to_string(),
            PathBuf::from(ws),
            AgentPhase::Code,
        );
        c.policy = PathPolicy::new(Path::new(ws));
        c
    }

    #[test]
    fn ssh_keys_stay_blocked_even_in_sovereign() {
        // SOVEREIGN skips the prompt, so the path sandbox is the only thing standing between a
        // prompt-injected `read` and a private key. It must deny on its own.
        let mut c = ctx_with(AccessMode::Sovereign, "/tmp/spavn-sec-ws");
        let home = crate::config::home_dir();
        for rel in [".ssh/id_rsa", ".spavn/secrets.env", ".aws/credentials"] {
            let path = home.join(rel);
            let args = serde_json::json!({ "path": path.to_string_lossy() }).to_string();
            let out = execute_tool(&mut c, "read", &args);
            assert!(
                out.contains("BLOCKED"),
                "{} must be denied, got: {}",
                rel,
                out.chars().take(120).collect::<String>()
            );
        }
    }

    #[test]
    fn a_host_that_ssh_reads_as_an_option_is_rejected() {
        // `ssh -o ConnectTimeout=8 <host> <cmd>` parses <host>, so a leading `-` is a local
        // command execution primitive (ProxyCommand), not a destination.
        let mut c = ctx_with(AccessMode::Sovereign, "/tmp/spavn-sec-ws");
        let args = serde_json::json!({
            "command": "echo hi",
            "host": "-oProxyCommand=touch /tmp/spavn-sec-pwned"
        })
        .to_string();
        let out = execute_tool(&mut c, "bash", &args);
        assert!(out.contains("Host Policy"), "{out}");
        assert!(
            !Path::new("/tmp/spavn-sec-pwned").exists(),
            "the injected ProxyCommand must never run"
        );
    }

    #[test]
    fn remote_search_rejects_an_option_style_host() {
        let mut c = ctx_with(AccessMode::Sovereign, "/tmp/spavn-sec-ws");
        let args = serde_json::json!({
            "pattern": "x",
            "path": "src",
            "host": "-oProxyCommand=id"
        })
        .to_string();
        let out = execute_tool(&mut c, "search", &args);
        assert!(out.contains("Host Policy"), "{out}");
    }

    #[test]
    fn background_logs_do_not_land_in_world_writable_tmp() {
        // A background `bash` used to write its log and stderr into $TMPDIR, which is shared and
        // predictable: another local user could pre-create the path (symlink) and read or
        // redirect the output. They now live under the 0700 spavn home.
        let mut c = ctx_with(AccessMode::Sovereign, "/tmp/spavn-sec-ws");
        let args =
            serde_json::json!({ "command": "echo sec-probe", "background": true }).to_string();
        let out = execute_tool(&mut c, "bash", &args);
        assert!(out.contains(".spavn/bg/"), "{out}");
        let log = out
            .split('`')
            .find(|s| s.contains(".log"))
            .unwrap_or("")
            .to_string();
        assert!(!log.is_empty(), "no log path in: {out}");
        assert!(
            !log.starts_with("/tmp/") && !log.contains("/var/folders/"),
            "log leaked into a shared temp dir: {log}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&log).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "background log must be owner-only");
        }
        let _ = fs::remove_file(&log);
    }

    #[test]
    fn an_unanswerable_approval_denies_the_action() {
        // No TTY means no one can answer the prompt. Blocking forever would hang the turn, and
        // "no answer" must never be read as consent.
        assert!(
            !crate::hud::stdin_is_tty(),
            "this test needs a non-tty stdin"
        );
        let dir = std::env::temp_dir().join(format!("spavn-sec-approve-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("would-be-written.txt");
        let mut c = ctx_with(AccessMode::Safe, &dir.to_string_lossy());
        let args = serde_json::json!({
            "path": target.to_string_lossy(),
            "content": "denied"
        })
        .to_string();
        let out = execute_tool(&mut c, "write", &args);
        assert!(out.contains("rejected") || out.contains("BLOCKED"), "{out}");
        assert!(!target.exists(), "a denied write must not touch the disk");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn web_refuses_a_private_target_before_any_socket_opens() {
        let mut c = ctx_with(AccessMode::Sovereign, "/tmp");
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/admin",
            "http://192.168.1.1/",
            "http://127.0.0.1.nip.io/",
            "http://[::1]:11434/api/tags",
        ] {
            let args = serde_json::json!({ "op": "fetch", "url": url }).to_string();
            let out = execute_tool(&mut c, "web", &args);
            assert!(out.contains("Egress Policy"), "{url} → {out}");
        }
    }

    #[test]
    fn web_search_backend_is_chosen_by_config_not_hardcoded() {
        // No `webSearchServer` keeps the built-in endpoint; naming one routes through that MCP
        // server, so the default host is never baked into the binary's behaviour.
        assert_eq!(resolve_web_search_target(None, None, |_| false), Ok(None));
        assert_eq!(
            resolve_web_search_target(Some("  "), Some("x"), |_| true),
            Ok(None)
        );
        assert_eq!(
            resolve_web_search_target(Some("lazyweb"), Some(" search "), |n| n == "lazyweb"),
            Ok(Some(("lazyweb".to_string(), "search".to_string())))
        );
        // A broken config must error instead of quietly falling back to the hosted default.
        let missing =
            resolve_web_search_target(Some("nope"), Some("search"), |_| false).unwrap_err();
        assert!(
            missing.contains("nope") && missing.contains("mcpServers"),
            "{missing}"
        );
        let no_tool = resolve_web_search_target(Some("lazyweb"), None, |_| true).unwrap_err();
        assert!(no_tool.contains("webSearchTool"), "{no_tool}");
    }

    #[test]
    fn web_http_backend_honours_tool_overrides() {
        // A self-hosted bridge names its own tools; unset or blank means "not wired up", never a
        // guessed name borrowed from some hosted vendor's catalogue.
        assert_eq!(web_http_tool(None), None);
        assert_eq!(web_http_tool(Some("   ")), None);
        assert_eq!(web_http_tool(Some(" search ")), Some("search".to_string()));
    }

    #[test]
    fn missing_search_backend_names_both_ways_to_wire_it_up() {
        // Nothing is baked in, so the refusal is the onboarding text: it has to point at the stdio
        // route and the HTTP route instead of implying a hidden default host.
        assert!(
            NO_SEARCH_BACKEND.contains("webSearchServer"),
            "{NO_SEARCH_BACKEND}"
        );
        assert!(
            NO_SEARCH_BACKEND.contains("webSearchUrl"),
            "{NO_SEARCH_BACKEND}"
        );
    }

    #[test]
    fn web_fetch_asks_in_safe_mode() {
        // Sovereign egress to a public host is unattended by design; SAFE must still ask, and
        // with no TTY that ask fails closed.
        let mut c = ctx_with(AccessMode::Safe, "/tmp");
        let args = serde_json::json!({ "op": "fetch", "url": "https://example.com/" }).to_string();
        let out = execute_tool(&mut c, "web", &args);
        assert!(out.contains("reddedildi") || out.contains("GLED"), "{out}");
    }

    #[test]
    fn mcp_mutating_tool_is_treated_as_mutating_by_default() {
        // The gate used to be a deny-list of verbs, so `commit`/`move_file` slipped through.
        for tool in [
            "commit",
            "move_file",
            "create_pull_request",
            "merge_pull_request",
            "delete_branch",
        ] {
            assert!(is_mutating_mcp_tool("some-server", tool, false), "{tool}");
        }
        for tool in ["get_page", "list_issues", "search_code", "read_file"] {
            assert!(!is_mutating_mcp_tool("some-server", tool, false), "{tool}");
        }
        // A server that declares itself read-only skips the gate entirely.
        assert!(!is_mutating_mcp_tool("docs", "commit", true));
    }

    #[test]
    fn mcp_response_is_capped() {
        let huge = format!(
            "{{\"result\":{{\"content\":[{{\"text\":\"{}\"}}]}}}}",
            "x".repeat(400_000)
        );
        let out = format_mcp_jsonrpc_response("srv", "tool", &huge);
        assert!(out.contains("truncated"), "{} bytes", out.len());
        assert!(out.len() < 200_000, "{} bytes", out.len());
    }
}

#[cfg(test)]
mod mcp_computer_tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            std::path::PathBuf::from("/tmp"),
            crate::spavn_engine::AgentPhase::Code,
        )
    }

    /// `computer` used to be a builtin that shelled out to a hardcoded `~/bin/sov` path. Desktop
    /// control is now just an MCP server the user configures, so the builtin must stay gone while
    /// the `lsp` tool keeps carrying the MCP client.
    #[test]
    fn computer_is_no_longer_a_builtin_tool() {
        let names: Vec<String> = tool_definitions()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
            .collect();
        assert!(!names.iter().any(|n| n == "computer"), "{:?}", names);
        assert!(names.iter().any(|n| n == "lsp"));

        // The model has to be able to find a server's tools without a second prompt, so the
        // description must carry the configured server names.
        let lsp = tool_definitions()
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == "lsp")
            .cloned()
            .unwrap();
        let desc = lsp["function"]["description"].as_str().unwrap();
        assert!(desc.contains("op='mcp'"), "{}", desc);
        let configured = crate::config::load_user_config();
        for name in configured.mcp_servers.keys() {
            assert!(
                desc.contains(name.as_str()),
                "{} missing from {}",
                name,
                desc
            );
        }
    }

    #[test]
    fn a_removed_builtin_reports_unknown() {
        let mut c = ctx();
        let out = execute_tool(&mut c, "computer", "{}");
        assert!(out.contains("Unknown tool"), "{}", out);
        assert!(!out.contains("computer,"), "stale hint: {}", out);
    }

    /// Exercises the real stdio transport against a throwaway server. Takes the config by value so
    /// the test never touches `~/.spavn/config.json`.
    #[test]
    fn stdio_mcp_lists_and_calls() {
        let dir = std::env::temp_dir().join(format!("spavn-mcp-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let server = dir.join("stub.py");
        std::fs::write(
            &server,
            r#"import json,sys
for line in sys.stdin:
    msg=json.loads(line)
    if "id" not in msg: continue
    m=msg.get("method")
    if m=="initialize":
        out={"protocolVersion":"2024-11-05","capabilities":{"tools":{}}}
    elif m=="tools/list":
        out={"tools":[{"name":"click","description":"Click","inputSchema":{"type":"object"}}]}
    elif m=="tools/call":
        a=msg["params"].get("arguments",{})
        out={"content":[{"type":"text","text":"clicked %s,%s"%(a.get("x"),a.get("y"))}]}
    else: continue
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":out})+"\n")
    sys.stdout.flush()
"#,
        )
        .unwrap();
        let srv = crate::config::McpServerConfig {
            command: Some("python3".to_string()),
            args: vec![server.display().to_string()],
            env: Default::default(),
            url: None,
            read_only: false,
        };

        let listed = call_stdio_mcp("desktop", &srv, "", &json!({}));
        let called = call_stdio_mcp("desktop", &srv, "click", &json!({"x": 3, "y": 4}));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(listed.contains("click"), "list: {}", listed);
        assert!(called.contains("clicked 3,4"), "call: {}", called);
    }

    /// The real desktop server, used only when the operator has it configured. `list_apps` is
    /// read-only, so this proves the whole path without touching the screen. The entry comes from
    /// the user config rather than a hardcoded path, so a launcher wrapper is exercised too.
    #[test]
    #[ignore]
    fn real_open_computer_use_lists_tools() {
        let Some(srv) = crate::config::load_user_config()
            .mcp_servers
            .get("open-computer-use")
            .cloned()
        else {
            return;
        };
        let listed = call_stdio_mcp("open-computer-use", &srv, "", &json!({}));
        assert!(listed.contains("click"), "{}", listed);
        assert!(listed.contains("get_app_state"), "{}", listed);
        let apps = call_stdio_mcp("open-computer-use", &srv, "list_apps", &json!({}));
        assert!(!apps.is_empty(), "{}", apps);
    }

    #[test]
    fn a_missing_server_binary_is_named() {
        let srv = crate::config::McpServerConfig {
            command: Some("spavn-no-such-binary".to_string()),
            args: Vec::new(),
            env: Default::default(),
            url: None,
            read_only: false,
        };
        let out = call_stdio_mcp("desktop", &srv, "", &json!({}));
        assert!(out.contains("desktop"), "{}", out);
    }
}

#[cfg(test)]
mod harness_intelligence_tests {
    use super::*;
    use crate::spavn_engine::AgentPhase;

    #[test]
    fn native_search_and_glob_work_without_rg() {
        let dir = std::env::temp_dir().join(format!("spavn-native-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/screens")).unwrap();
        fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        fs::write(
            dir.join("src/screens/Billing.tsx"),
            "const x = 1;\nconst res = await uploadInboxFile(token);\n",
        )
        .unwrap();
        fs::write(
            dir.join("node_modules/pkg/index.js"),
            "uploadInboxFile();\n",
        )
        .unwrap();

        let globbed = native_glob_fallback(&dir, "**/*.tsx", 50, false);
        assert!(globbed.contains("Billing.tsx"), "{globbed}");
        assert!(!globbed.contains("node_modules"), "{globbed}");

        let searched =
            native_search_fallback(&dir, "UploadInboxFile\\|uploadInboxFile", "*.tsx", 1);
        assert!(searched.contains("Billing.tsx:2:"), "{searched}");
        assert!(
            searched.contains("Billing.tsx-1-const x = 1;"),
            "{searched}"
        );
        assert!(!searched.contains("node_modules"), "{searched}");
        assert!(is_structural_outline_line(
            "  const handleSendRequest = async () => {"
        ));
        assert!(is_structural_outline_line(
            "    private String getMimeType(String url) {"
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bash_interceptor_blocks_recursive_grep_find_and_standalone_cat() {
        assert!(intercept_shell_anti_pattern("grep -rn \"UploadInboxFile\" src/").is_some());
        assert!(intercept_shell_anti_pattern(
            "grep -rn \"headless\" ~/.spavn 2>/dev/null | head -n 10"
        )
        .is_some());
        assert!(intercept_shell_anti_pattern("rg \"foo\" src/").is_some());
        assert!(intercept_shell_anti_pattern("find . -name \"*.rs\"").is_some());
        assert!(
            intercept_shell_anti_pattern("find /tmp -name \"*.log\" 2>/dev/null | head -n 20")
                .is_some()
        );
        assert!(intercept_shell_anti_pattern("cat src/main.rs").is_some());
        assert!(intercept_shell_anti_pattern("sed -n '10,40p' src/main.rs").is_some());
        // Legitimate shell commands and pipelines pass untouched
        assert!(intercept_shell_anti_pattern("git status -s && git log -n 5").is_none());
        assert!(intercept_shell_anti_pattern("cargo test --quiet").is_none());
        assert!(intercept_shell_anti_pattern("sed -i '' 's/a/b/g' foo.txt").is_none());
        assert!(intercept_shell_anti_pattern("ps aux | grep spavn").is_none());
    }

    #[test]
    fn read_edit_search_resolve_relative_paths_against_last_cwd_and_sqlite_query_stays_intact() {
        let root_a = std::env::temp_dir().join(format!("spavn-cwd-a-{}", std::process::id()));
        let root_b = std::env::temp_dir().join(format!("spavn-cwd-b-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root_a);
        let _ = fs::remove_dir_all(&root_b);
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(root_b.join("sub")).unwrap();
        fs::write(root_b.join("sub/hello.txt"), "alpha\nbeta\ngamma\n").unwrap();

        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            root_a.clone(),
            AgentPhase::Code,
        );
        // Simulate `bash` having `cd`'d into `root_b` while process CWD is still elsewhere
        c.last_cwd = root_b.clone();

        let read_out = exec_read(&mut c, &json!({ "path": "sub/hello.txt" }));
        assert!(read_out.contains("beta"), "read output: {read_out}");

        let edit_out = exec_edit(
            &mut c,
            &json!({ "path": "sub/hello.txt", "old_text": "beta", "new_text": "BETA_UPDATED" }),
        );
        assert!(
            edit_out.starts_with("Successfully edited"),
            "edit output: {edit_out}"
        );

        let search_out = exec_search(&mut c, &json!({ "pattern": "beta_updated", "path": "sub" }));
        assert!(
            search_out.contains("BETA_UPDATED"),
            "search output: {search_out}"
        );

        let _ = fs::remove_dir_all(&root_a);
        let _ = fs::remove_dir_all(&root_b);
    }

    #[test]
    fn artifact_shield_blocks_compiled_bundles_and_allows_normal_sources() {
        let huge_bundle = "var a = 1;\n".repeat(4_000);
        assert!(
            check_compiled_artifact_shield("dist/index.android.bundle", &huge_bundle).is_some()
        );
        assert!(check_compiled_artifact_shield("src/screens/Billing.tsx", &huge_bundle).is_none());
    }

    #[test]
    fn local_model_micro_schema_prunes_domain_tools_until_requested() {
        let default_local =
            tool_definitions_for_turn(AgentPhase::Code, true, "fix the bug in main.rs");
        let arr = default_local.as_array().unwrap();
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|v| v["function"]["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec!["read", "edit", "write", "bash", "search", "glob"]
        );

        let with_mem_and_web = tool_definitions_for_turn(
            AgentPhase::Code,
            true,
            "bunu hafızaya kaydet ve https://spavn.dev adresine bak",
        );
        let names2: Vec<&str> = with_mem_and_web
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v["function"]["name"].as_str())
            .collect();
        assert!(names2.contains(&"memory"));
        assert!(names2.contains(&"web"));
    }

    #[test]
    fn cross_file_atomic_edit_applies_and_rolls_back_on_error_with_impact_graph() {
        let dir = std::env::temp_dir().join(format!("spavn-xfile-edit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let api_file = dir.join("api.ts");
        let screen_file = dir.join("Screen.tsx");
        fs::write(
            &api_file,
            "export const uploadInvoice = async (id: string) => {\n  return id;\n};\n",
        )
        .unwrap();
        fs::write(
            &screen_file,
            "import { uploadInvoice } from './api';\nexport function renderScreen() {\n  return uploadInvoice('1');\n}\n",
        )
        .unwrap();

        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            dir.clone(),
            AgentPhase::Code,
        );
        // 1. Failing 2nd file rolls back 1st file atomically
        let bad_tx = json!({
            "files": [
                {
                    "path": api_file.to_string_lossy(),
                    "old_text": "return id;",
                    "new_text": "return id.trim();"
                },
                {
                    "path": screen_file.to_string_lossy(),
                    "old_text": "NON_EXISTENT_ANCHOR",
                    "new_text": "x"
                }
            ]
        });
        let err_out = exec_edit(&mut c, &bad_tx);
        assert!(err_out.contains("atomic rollback"), "{err_out}");
        assert!(
            fs::read_to_string(&api_file)
                .unwrap()
                .contains("return id;\n"),
            "api.ts was not rolled back!"
        );

        // 2. Valid cross-file edit succeeds and emits Impact Graph hint for `uploadInvoice`
        let ok_tx = json!({
            "files": [
                {
                    "path": api_file.to_string_lossy(),
                    "old_text": "export const uploadInvoice = async (id: string) => {",
                    "new_text": "export const uploadInvoice = async (id: string, token?: string) => {"
                },
                {
                    "path": screen_file.to_string_lossy(),
                    "old_text": "return uploadInvoice('1');",
                    "new_text": "return uploadInvoice('1', 'tok');"
                }
            ]
        });
        let ok_out = exec_edit(&mut c, &ok_tx);
        assert!(
            ok_out.starts_with("Successfully edited 2 file(s) atomically"),
            "{ok_out}"
        );
        assert!(ok_out.contains("SPAVN Impact Graph"), "{ok_out}");
        assert!(ok_out.contains("Screen.tsx"), "{ok_out}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_memory_multi_hunk_edit_with_line_delta_and_token_free_ranges() {
        let dir = std::env::temp_dir().join(format!("spavn-hunk-delta-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("sample.rs");
        fs::write(
            &f,
            "fn one() {\n    let a = 1;\n}\nfn two() {\n    let b = 2;\n}\n",
        )
        .unwrap();

        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            dir.clone(),
            AgentPhase::Code,
        );

        // Multi-hunk edit using original line numbers: hunk #1 expands line 2 into 3 lines (+2 delta),
        // hunk #2 still targets original line 5 (`let b = 2;`) without the caller recalculating offsets!
        let out = exec_edit(
            &mut c,
            &json!({
                "path": f.to_string_lossy(),
                "edits": [
                    {
                        "start_line": 2,
                        "end_line": 2,
                        "new_text": "    let a = 10;\n    let a2 = 20;\n    let a3 = 30;"
                    },
                    {
                        "start_line": 5,
                        "end_line": 5,
                        "new_text": "    let b = 200;"
                    }
                ]
            }),
        );
        assert!(out.contains("2 hunks applied in-memory"), "{out}");
        let updated = fs::read_to_string(&f).unwrap();
        assert!(updated.contains("let a3 = 30;"), "{updated}");
        assert!(updated.contains("let b = 200;"), "{updated}");
        assert!(!updated.contains("let b = 2;"), "{updated}");

        // Also test inline `path:1-1` and `insert_after_line`
        let out2 = exec_edit(
            &mut c,
            &json!({
                "path": format!("{}:1-1", f.to_string_lossy()),
                "new_text": "pub fn one() {"
            }),
        );
        assert!(out2.contains("Successfully edited"), "{out2}");
        let out3 = exec_edit(
            &mut c,
            &json!({
                "path": f.to_string_lossy(),
                "insert_after_line": 0,
                "new_text": "// header comment"
            }),
        );
        assert!(out3.contains("Successfully edited"), "{out3}");
        let final_text = fs::read_to_string(&f).unwrap();
        assert!(
            final_text.starts_with("// header comment\npub fn one() {"),
            "{final_text}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_bm25_ast_semantic_fallback_with_turkish_normalization() {
        let dir = std::env::temp_dir().join(format!("spavn-bm25-search-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("invoice_service.rs"),
            "pub fn fatura_yukle_ve_dogrula(musteri_id: &str) -> bool {\n    let durum = kredi_kontrol(musteri_id);\n    durum\n}\n",
        )
        .unwrap();

        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            dir.clone(),
            AgentPhase::Code,
        );
        // Query uses Turkish diacritics and natural word order not matching literal substring
        let out = exec_search(
            &mut c,
            &json!({
                "pattern": "müşteri fatura yükle doğrula",
                "path": dir.to_string_lossy()
            }),
        );
        assert!(out.contains("BM25+AST"), "{out}");
        assert!(out.contains("fatura_yukle_ve_dogrula"), "{out}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bash_lang_py_stdin_execution_without_quote_hell() {
        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            std::env::temp_dir(),
            AgentPhase::Code,
        );
        let script = "import json\ndata = {'quote': \"it's \\\"fine\\\"\", 'n': 42}\nprint(json.dumps(data, sort_keys=True))";
        let out = exec_bash(
            &mut c,
            &json!({
                "lang": "py",
                "command": script
            }),
        );
        assert!(
            out.contains(r#"{"n": 42, "quote": "it's \"fine\""}"#),
            "{out}"
        );
    }

    #[test]
    fn read_tail_and_json_pointer_zero_latency_selectors() {
        let dir = std::env::temp_dir().join(format!("spavn-read-sel-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log_file = dir.join("app.log");
        let lines: Vec<String> = (1..=20).map(|i| format!("line-{i}")).collect();
        fs::write(&log_file, lines.join("\n")).unwrap();

        let json_file = dir.join("config.json");
        fs::write(
            &json_file,
            r#"{"router":{"primary":"argon","fallbacks":["local","flash"]}}"#,
        )
        .unwrap();

        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            dir.clone(),
            AgentPhase::Code,
        );
        let tail_out = exec_read(
            &mut c,
            &json!({
                "path": format!("{}:-3", log_file.to_string_lossy())
            }),
        );
        assert!(tail_out.contains("18: line-18"), "{tail_out}");
        assert!(tail_out.contains("20: line-20"), "{tail_out}");
        assert!(!tail_out.contains("17: line-17"), "{tail_out}");

        let jq_out = exec_read(
            &mut c,
            &json!({
                "path": format!("{}?q=.router.fallbacks.1", json_file.to_string_lossy())
            }),
        );
        assert!(jq_out.contains("\"flash\""), "{jq_out}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fs_stale_version_blocks_blind_overwrite_when_file_changed_externally() {
        let dir = std::env::temp_dir().join(format!("spavn-stale-guard-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("state.rs");
        fs::write(&file, "fn alpha() -> i32 {\n    1\n}\n").unwrap();

        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            dir.clone(),
            AgentPhase::Code,
        );
        let file_str = file.to_string_lossy().to_string();
        let _ = exec_read(&mut c, &json!({ "path": file_str }));
        assert!(!c.is_external_stale_version(&file_str));

        // Simulate external modification after `read` (even within the same millisecond)
        fs::write(&file, "fn alpha() -> i32 {\n    42\n}\n").unwrap();
        assert!(c.is_external_stale_version(&file_str));

        let blocked_write = execute_tool(
            &mut c,
            "write",
            &serde_json::to_string(&json!({
                "path": file_str,
                "content": "fn alpha() -> i32 { 0 }\n"
            }))
            .unwrap(),
        );
        assert!(
            blocked_write.contains("FS_STALE_VERSION"),
            "{blocked_write}"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}

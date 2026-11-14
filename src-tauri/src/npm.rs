// npm global updates without running other people's code by default.
//
// `npm install -g <pkg>@latest` runs the package's — and every dependency's — preinstall /
// install / postinstall scripts with the user's full access: the classic npm supply-chain
// vector. It also installs whatever "latest" is at that moment, not the version that was shown.
// So Freshet:
//   1. installs the exact version the list showed (`name@version`), never a moving tag;
//   2. asks the registry, before installing, whether that version declares install scripts —
//      if it does and the user hasn't approved that exact name@version, nothing is installed and
//      the result shows the scripts with an "Allow install scripts" action;
//   3. otherwise installs with --ignore-scripts, so no dependency script runs either, and then
//      reports any dependency scripts that were skipped (in case the tool needs them), with the
//      same approve action.
// Approving reinstalls that exact version with scripts. Approval is per name@version, so a new
// release with new scripts asks again.

use crate::{ResultAction, UpdateResult, UpdateSelection};

const INSTALL_SCRIPTS: [&str; 3] = ["preinstall", "install", "postinstall"];

// `npm view <spec> scripts --json` → the install-time scripts, in the order npm runs them.
// Pure → unit-tested.
pub(crate) fn parse_install_scripts(json: &str) -> Vec<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    INSTALL_SCRIPTS
        .iter()
        .filter_map(|k| {
            v[*k]
                .as_str()
                .map(|cmd| ((*k).to_string(), cmd.to_string()))
        })
        .collect()
}

// `npm query -g ':attr(scripts, [postinstall]), …' ` → "dep: postinstall: cmd" for packages in
// `name`'s own install tree (the package itself or anything under its node_modules). Pure.
pub(crate) fn skipped_scripts_in(query_json: &str, name: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(query_json).unwrap_or_default();
    let own = format!("/node_modules/{name}");
    v.as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|p| {
            p["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(&own) || path.contains(&format!("{own}/")))
        })
        .flat_map(|p| {
            let pkg = p["name"].as_str().unwrap_or("?").to_string();
            INSTALL_SCRIPTS
                .iter()
                .filter_map(|k| {
                    p["scripts"][*k]
                        .as_str()
                        .map(|cmd| format!("{pkg}: {k}: {cmd}"))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

// Exact version, and scripts only when approved. Pure → unit-tested.
pub(crate) fn install_args(spec: &str, run_scripts: bool) -> Vec<String> {
    let mut args = vec!["install".to_string(), "-g".to_string()];
    if !run_scripts {
        args.push("--ignore-scripts".to_string());
    }
    args.push(spec.to_string());
    args
}

// A version we're willing to put on a command line: semver-ish characters only, and starting with
// a digit, so a dist-tag like "latest" or "next" (which moves) can never stand in for it.
pub(crate) fn is_valid_version(v: &str) -> bool {
    v.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && v.len() <= 64
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
}

fn result(sel: &UpdateSelection, success: bool, output: String) -> UpdateResult {
    UpdateResult {
        kind: sel.kind.clone(),
        name: sel.name.clone(),
        success,
        output,
        action: None,
        elapsed_ms: None,
        skipped: false,
        steps: Vec::new(),
    }
}

fn approve_action(name: &str, version: &str, scripts: &[String]) -> ResultAction {
    ResultAction::AllowNpmScripts {
        name: name.to_string(),
        version: version.to_string(),
        scripts: scripts.join("\n"),
    }
}

// Update one npm global. `npm` is the program to run (the real one, or a stand-in in tests),
// `version` the version the list showed, `approved` the user's approved "name@version"s.
pub(crate) fn update(
    sel: &UpdateSelection,
    version: Option<&str>,
    approved: &[String],
    npm: &str,
) -> UpdateResult {
    let name = sel.id.as_str();
    if !crate::is_valid_npm_package(name) {
        return result(sel, false, format!("invalid npm package name {name:?}"));
    }
    let Some(version) = version.filter(|v| is_valid_version(v)) else {
        return result(
            sel,
            false,
            "Not installed: couldn't tell which version the list showed. Refresh and try again."
                .into(),
        );
    };
    let spec = format!("{name}@{version}");
    let allowed = approved.contains(&spec);

    if !allowed {
        // What the registry says this exact version runs at install time.
        let declared = crate::run_cmd(npm, &["view", &spec, "scripts", "--json"])
            .map(|json| parse_install_scripts(&json))
            .unwrap_or_default();
        if !declared.is_empty() {
            let listed: Vec<String> = declared
                .iter()
                .map(|(k, cmd)| format!("{k}: {cmd}"))
                .collect();
            return UpdateResult {
                action: Some(approve_action(name, version, &listed)),
                skipped: true,
                steps: Vec::new(),
                ..result(
                    sel,
                    false,
                    format!(
                        "Not installed yet: {spec} runs install scripts, which run with your \
                         account's full access:\n{}\nAllow them only if you trust this package.",
                        listed.join("\n")
                    ),
                )
            };
        }
    }

    let args = install_args(&spec, allowed);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let started = std::time::Instant::now();
    let (success, mut output) = crate::run_update_cmd(npm, &arg_refs);
    let mut r = UpdateResult {
        elapsed_ms: Some(crate::elapsed_ms(started)),
        ..result(sel, success, String::new())
    };

    if success && !allowed {
        // Which of its dependencies wanted to run scripts that --ignore-scripts skipped.
        let selector = INSTALL_SCRIPTS
            .iter()
            .map(|k| format!(":attr(scripts, [{k}])"))
            .collect::<Vec<_>>()
            .join(", ");
        let skipped = crate::run_cmd(npm, &["query", "-g", &selector])
            .map(|json| skipped_scripts_in(&json, name))
            .unwrap_or_default();
        if !skipped.is_empty() {
            output = format!(
                "{output}\n\nInstalled without running these install scripts:\n{}\nIf {name} \
                 doesn't work, allow them to reinstall {version} with scripts.",
                skipped.join("\n")
            )
            .trim()
            .to_string();
            r.action = Some(approve_action(name, version, &skipped));
        }
    }
    r.output = output;
    r
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn install_scripts_from_npm_view() {
        // Real `npm view esbuild@0.25.0 scripts --json` output (trimmed).
        let esbuild = r#"{"postinstall": "node install.js"}"#;
        assert_eq!(
            parse_install_scripts(esbuild),
            [("postinstall".to_string(), "node install.js".to_string())]
        );
        // pnpm 12 declares both, run preinstall first.
        let pnpm =
            r#"{"test": "x", "postinstall": "node install.js", "preinstall": "node install.js"}"#;
        let keys: Vec<String> = parse_install_scripts(pnpm)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            keys,
            ["preinstall", "postinstall"],
            "non-install scripts ignored, npm's order"
        );
        assert_eq!(
            parse_install_scripts(""),
            Vec::<(String, String)>::new(),
            "no scripts field"
        );
        assert_eq!(parse_install_scripts("{}"), Vec::<(String, String)>::new());
    }

    #[test]
    fn skipped_scripts_are_scoped_to_the_package() {
        let query = r#"[
          {"name":"pnpm","path":"/Users/u/.nvm/versions/node/v20/lib/node_modules/pnpm","scripts":{"postinstall":"node install.js"}},
          {"name":"esbuild","path":"/Users/u/.nvm/versions/node/v20/lib/node_modules/vite/node_modules/esbuild","scripts":{"postinstall":"node install.js"}},
          {"name":"fsevents","path":"/Users/u/.nvm/versions/node/v20/lib/node_modules/typescript-tools/node_modules/fsevents","scripts":{"install":"node-gyp rebuild"}}
        ]"#;
        assert_eq!(
            skipped_scripts_in(query, "vite"),
            ["esbuild: postinstall: node install.js"]
        );
        assert_eq!(
            skipped_scripts_in(query, "pnpm"),
            ["pnpm: postinstall: node install.js"]
        );
        assert_eq!(
            skipped_scripts_in(query, "vit"),
            Vec::<String>::new(),
            "no prefix matches"
        );
        assert_eq!(skipped_scripts_in("garbage", "vite"), Vec::<String>::new());
    }

    #[test]
    fn install_args_pin_the_version_and_skip_scripts_unless_approved() {
        assert_eq!(
            install_args("typescript@6.0.2", false),
            ["install", "-g", "--ignore-scripts", "typescript@6.0.2"]
        );
        assert_eq!(
            install_args("pnpm@10.18.1", true),
            ["install", "-g", "pnpm@10.18.1"]
        );
    }

    #[test]
    fn version_validation() {
        assert!(is_valid_version("6.0.2"));
        assert!(is_valid_version("1.0.0-beta.1+build.5"));
        assert!(!is_valid_version("latest; rm -rf ~"));
        assert!(
            !is_valid_version("latest"),
            "a moving dist-tag is not a version"
        );
        assert!(!is_valid_version("next"));
        assert!(!is_valid_version(""));
    }

    // A stand-in `npm` that records every call and answers `view` / `query` from files.
    struct FakeNpm {
        dir: PathBuf,
    }

    impl FakeNpm {
        fn new(name: &str, view_scripts: &str, query: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("freshet-npm-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("view.json"), view_scripts).unwrap();
            std::fs::write(dir.join("query.json"), query).unwrap();
            let script = format!(
                "#!/bin/bash\necho \"$*\" >> '{d}/calls'\ncase \"$1\" in\n  view) cat '{d}/view.json' ;;\n  query) cat '{d}/query.json' ;;\nesac\nexit 0\n",
                d = dir.display()
            );
            crate::write_private(&dir.join("npm"), &script, true).unwrap();
            Self { dir }
        }
        fn program(&self) -> String {
            self.dir.join("npm").to_string_lossy().into_owned()
        }
        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        }
    }

    impl Drop for FakeNpm {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn sel(id: &str) -> UpdateSelection {
        UpdateSelection {
            kind: "npm".into(),
            id: id.into(),
            name: id.into(),
        }
    }

    fn installs(calls: &[String]) -> Vec<&String> {
        calls.iter().filter(|c| c.starts_with("install")).collect()
    }

    #[test]
    fn package_without_scripts_installs_exact_version_with_scripts_disabled() {
        let npm = FakeNpm::new("plain", "", "[]");
        let r = update(&sel("typescript"), Some("6.0.2"), &[], &npm.program());
        assert!(r.success && r.action.is_none(), "{r:?}");
        let calls = npm.calls();
        assert_eq!(calls[0], "view typescript@6.0.2 scripts --json");
        assert_eq!(
            installs(&calls),
            ["install -g --ignore-scripts typescript@6.0.2"]
        );
    }

    #[test]
    fn package_with_install_scripts_is_not_installed_until_approved() {
        let npm = FakeNpm::new(
            "scripts",
            r#"{"preinstall":"node install.js","postinstall":"node install.js"}"#,
            "[]",
        );
        let r = update(&sel("pnpm"), Some("10.18.1"), &[], &npm.program());
        assert!(!r.success && r.skipped, "{r:?}");
        assert!(
            installs(&npm.calls()).is_empty(),
            "nothing installed: {:?}",
            npm.calls()
        );
        assert!(
            r.output.contains("preinstall: node install.js"),
            "{}",
            r.output
        );
        assert!(matches!(
            r.action,
            Some(ResultAction::AllowNpmScripts { ref name, ref version, .. }) if name == "pnpm" && version == "10.18.1"
        ));
    }

    #[test]
    fn approved_version_installs_with_scripts_and_skips_the_registry_check() {
        let npm = FakeNpm::new("approved", r#"{"postinstall":"node install.js"}"#, "[]");
        let r = update(
            &sel("pnpm"),
            Some("10.18.1"),
            &["pnpm@10.18.1".into()],
            &npm.program(),
        );
        assert!(r.success, "{r:?}");
        assert_eq!(npm.calls(), ["install -g pnpm@10.18.1"]);
    }

    // Approval is for one exact version: a newer release (maybe with different scripts) asks again.
    #[test]
    fn approval_does_not_carry_over_to_another_version() {
        let npm = FakeNpm::new("newver", r#"{"postinstall":"node evil.js"}"#, "[]");
        let r = update(
            &sel("pnpm"),
            Some("10.19.0"),
            &["pnpm@10.18.1".into()],
            &npm.program(),
        );
        assert!(!r.success && r.output.contains("node evil.js"), "{r:?}");
        assert_eq!(installs(&npm.calls()), Vec::<&String>::new());
    }

    #[test]
    fn skipped_dependency_scripts_are_reported_with_an_approve_action() {
        let query = format!(
            r#"[{{"name":"esbuild","path":"{}","scripts":{{"postinstall":"node install.js"}}}}]"#,
            Path::new("/u/lib/node_modules/vite/node_modules/esbuild").display()
        );
        let npm = FakeNpm::new("deps", "{}", &query);
        let r = update(&sel("vite"), Some("7.3.7"), &[], &npm.program());
        assert!(r.success, "{r:?}");
        assert!(
            r.output.contains("esbuild: postinstall: node install.js"),
            "{}",
            r.output
        );
        assert!(matches!(
            r.action,
            Some(ResultAction::AllowNpmScripts { .. })
        ));
        assert_eq!(
            installs(&npm.calls()),
            ["install -g --ignore-scripts vite@7.3.7"]
        );
    }

    #[test]
    fn refuses_without_a_known_version_instead_of_installing_latest() {
        let npm = FakeNpm::new("nover", "", "[]");
        let r = update(&sel("typescript"), None, &[], &npm.program());
        assert!(!r.success);
        assert!(npm.calls().is_empty(), "npm never ran");
        let r = update(
            &sel("typescript"),
            Some("latest; rm -rf ~"),
            &[],
            &npm.program(),
        );
        assert!(!r.success && npm.calls().is_empty());
    }

    // Real npm, read-only (`npm view`, `npm query`): what Freshet would see for pnpm.
    //   cargo test -- --ignored npm_live --nocapture
    #[test]
    #[ignore = "queries the npm registry and the real global install"]
    fn npm_live_scripts_visible_before_and_after() {
        let latest = crate::run_cmd("npm", &["view", "pnpm", "version"]).unwrap();
        let spec = format!("pnpm@{}", latest.trim());
        let declared = parse_install_scripts(
            &crate::run_cmd("npm", &["view", &spec, "scripts", "--json"]).unwrap(),
        );
        eprintln!("{spec} declares: {declared:?}");
        assert!(
            !declared.is_empty(),
            "pnpm declares install scripts, so its update would wait for approval"
        );
        let selector = INSTALL_SCRIPTS
            .map(|k| format!(":attr(scripts, [{k}])"))
            .join(", ");
        let query = crate::run_cmd("npm", &["query", "-g", &selector]).unwrap();
        eprintln!(
            "installed pnpm tree scripts: {:?}",
            skipped_scripts_in(&query, "pnpm")
        );
    }
}

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "factsheet-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create isolated project");
        fs::create_dir_all(root.join("config/factsheet")).expect("create config directory");
        fs::write(root.join("config/factsheet/config.toml"), "").expect("seed config");
        Self(root)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_factsheet"));
        command
            .current_dir(&self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .args(args);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run factsheet")
    }

    fn success(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("UTF-8 output")
    }

    fn seed(&self, contents: &[u8]) -> PathBuf {
        let dir = self.0.join(".factsheet");
        fs::create_dir_all(&dir).expect("create store directory");
        let store = dir.join("facts.jsonl");
        fs::write(&store, contents).expect("seed facts");
        store
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("cannot remove test project {}: {error}", self.0.display());
        }
    }
}

#[test]
fn preserves_unreadable_config_and_reports_it_in_inject() {
    let project = Project::new();
    let config = project.0.join("config/factsheet/config.toml");
    let contents = b"user_instructions = \"\xff\"\n";
    fs::write(&config, contents).expect("seed invalid UTF-8");
    let output = project.success(&[]);
    assert!(output.contains("cannot read config"));
    assert!(output.contains("User instructions were not injected"));
    assert_eq!(fs::read(config).expect("read unchanged config"), contents);
}

#[test]
fn separates_new_facts_from_an_unfinished_last_line() {
    let project = Project::new();
    project.seed(b"{\"id\":");
    project.success(&["add", "New fact"]);
    assert!(project.success(&["ls"]).contains("New fact"));
}

#[test]
fn reports_store_read_errors_instead_of_empty_memory() {
    let project = Project::new();
    let store = project.seed(b"\xff");
    let output = project.run(&["ls"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read"));
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(store).expect("read unchanged store"), b"\xff");
}

#[test]
fn registers_facts_created_in_an_existing_directory() {
    let project = Project::new();
    fs::create_dir(project.0.join(".factsheet")).expect("create empty store directory");
    project.success(&["add", "New fact"]);
    assert!(project.success(&["projects"]).contains("1 facts"));
}

#[test]
fn counts_each_tag_once_per_fact() {
    let project = Project::new();
    project.seed(b"{\"id\":\"old\",\"text\":\"Old fact\",\"tags\":[\"deploy\",\"deploy\"],\"ts\":\"2026-09-05\"}\n");
    project.success(&["add", "New fact", "-t", "deploy,deploy"]);
    assert_eq!(project.success(&["tags"]), "#deploy 2\n");
    project.success(&["edit", "old", "-t", "deploy,deploy"]);
    assert_eq!(project.success(&["tags"]), "#deploy 2\n");
    let data = fs::read_to_string(project.0.join(".factsheet/facts.jsonl"))
        .expect("read normalized facts");
    assert!(!data.contains("\"deploy\",\"deploy\""));
}

#[test]
fn preserves_all_concurrent_add_edit_and_drop_operations() {
    let project = Project::new();
    project.seed(b"{\"id\":\"edit\",\"text\":\"Original\",\"ts\":\"2026-09-05\"}\n{\"id\":\"drop\",\"text\":\"Remove\",\"ts\":\"2026-09-05\"}\n");
    let lock = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(project.0.join(".factsheet/.lock"))
        .expect("create lock");
    fs2::FileExt::lock_exclusive(&lock).expect("hold lock");
    let mut children = vec![
        project.command(&["edit", "edit", "Updated"]),
        project.command(&["drop", "drop"]),
        project.command(&["add", "Added first"]),
        project.command(&["add", "Added second"]),
    ]
    .into_iter()
    .map(|mut command| {
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start concurrent writer")
    })
    .collect::<Vec<_>>();
    std::thread::sleep(Duration::from_millis(100));
    let blocked = children
        .iter_mut()
        .all(|child| child.try_wait().expect("check blocked writer").is_none());
    drop(lock);
    let deadline = Instant::now() + Duration::from_secs(10);
    for child in &mut children {
        loop {
            if let Some(status) = child.try_wait().expect("wait for writer") {
                assert!(status.success(), "writer failed: {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().expect("stop stalled writer");
                child.wait().expect("reap stalled writer");
                panic!("writer did not release the lock");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(blocked, "all mutations must wait for the store lock");
    let output = project.success(&["ls"]);
    assert_eq!(output.lines().count(), 3);
    for text in ["Updated", "Added first", "Added second"] {
        assert!(output.contains(text), "missing {text}");
    }
    assert!(!output.contains("Remove"));
}

#[test]
fn leaves_store_unchanged_when_backup_fails() {
    let project = Project::new();
    let original = b"{\"id\":\"old\",\"text\":\"Original\",\"ts\":\"2026-09-05\"}\n";
    let store = project.seed(original);
    fs::create_dir(store.with_file_name("facts.jsonl.bak")).expect("block backup path");
    let output = project.run(&["edit", "old", "Changed"]);
    assert!(!output.status.success());
    assert_eq!(fs::read(store).expect("read unchanged store"), original);
}

#[cfg(unix)]
#[test]
fn fails_on_full_stdout() {
    let project = Project::new();
    let full = std::path::Path::new("/dev/full");
    if !full.exists() {
        return;
    }
    let output = project
        .command(&[])
        .stdout(
            fs::OpenOptions::new()
                .write(true)
                .open(full)
                .expect("open /dev/full"),
        )
        .stderr(Stdio::piped())
        .output()
        .expect("run with failing stdout");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot write stdout"));
}

#[cfg(unix)]
#[test]
fn curate_applies_edits_drops_and_adds_from_the_sheet() {
    let project = Project::new();
    project.seed(b"{\"id\":\"keep\",\"text\":\"Old text\",\"tags\":[\"a\"],\"ts\":\"2026-01-01\"}\n{\"id\":\"gone\",\"text\":\"Remove me\",\"tags\":[\"b\"],\"ts\":\"2026-01-01\"}\n{\"id\":\"same\",\"text\":\"Untouched\",\"tags\":[\"ref\"],\"ts\":\"2026-01-01\"}\n");
    let editor = project.0.join("editor.sh");
    fs::write(
        &editor,
        "#!/bin/sh\nsed -e 's/Old text/New text/' -e '/Remove me/d' -e 's/Untouched  #ref/Untouched  #ref #c/' \"$1\" > \"$1.tmp\" && mv \"$1.tmp\" \"$1\"\necho 'Brand new | check: true  #d' >> \"$1\"\n",
    )
    .expect("write editor script");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).expect("chmod editor");
    let output = project
        .command(&["curate"])
        .env("EDITOR", &editor)
        .env_remove("VISUAL")
        .output()
        .expect("run curate");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "added 1, edited 2, dropped 1\n"
    );
    let listing = project.success(&["ls"]);
    assert!(listing.contains("[keep] New text  #a"));
    assert!(!listing.contains("Remove me"));
    assert!(listing.contains("[same] Untouched  #ref #c  (2026-01-01)"));
    assert!(listing.contains("Brand new | check: true  #d"));
    assert!(!project.0.join(".factsheet/curate.txt").exists());
}

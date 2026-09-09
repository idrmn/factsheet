use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(
    name = "factsheet",
    version,
    about = "Per-project agent memory: structured facts in .factsheet/facts.jsonl",
    long_about = "Per-project agent memory. Run with no arguments to print the whole \
store (session inject). Run `factsheet agent` for the full usage guide."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Add a new fact (text, --check, or both)
    Add {
        /// Fact text (optional when --check is given)
        text: Option<String>,
        /// Check command stored with the fact (never executed by factsheet)
        #[arg(short, long)]
        check: Option<String>,
        /// Comma-separated tags
        #[arg(short, long, value_delimiter = ',')]
        tags: Vec<String>,
    },
    /// Edit a fact: provided fields replace old ones, ts refreshes
    Edit {
        id: String,
        /// New fact text
        text: Option<String>,
        /// New check command
        #[arg(short, long)]
        check: Option<String>,
        /// New comma-separated tags (use -t "" to clear)
        #[arg(short, long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
    },
    /// Delete a fact
    Drop { id: String },
    /// List facts, optionally filtered
    Ls {
        /// Only facts carrying any of these tags (comma-separated)
        #[arg(short, long, value_delimiter = ',')]
        tag: Vec<String>,
        /// Only facts not written for N+ days (e.g. 30d)
        #[arg(long, value_name = "Nd")]
        stale: Option<String>,
    },
    /// List unique tags with fact counts
    Tags,
    /// Find facts most similar to a query (word overlap)
    Find {
        /// Query words (quotes optional)
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// List near-duplicate fact pairs for curation
    Dedup,
    /// Edit all facts in $EDITOR, grouped by tag (humans only)
    Curate,
    /// List every project where factsheet has a store (self-pruning registry)
    Projects,
    /// Print the usage guide for agents
    Agent,
}

#[derive(Serialize, Deserialize)]
struct Fact {
    id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    check: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    ts: String,
}

type Words = BTreeSet<String>;

impl Fact {
    fn is_ref(&self) -> bool {
        self.tags.iter().any(|t| t == "ref")
    }

    fn text_len(&self) -> usize {
        self.text.chars().count()
    }

    // Shared tags do not make fact contents similar.
    fn words(&self) -> Words {
        let mut words = tokenize(&self.text);
        words.extend(tokenize(&self.check));
        words
    }
}

const AGENT_GUIDE: &str = include_str!("agent-guide.md");

// Empty projects also need the memory write rules.
const INJECT_RULES: &str = "\
factsheet = persistent per-project memory (this CLI), injected each session start.
write: repeated error or gotcha a fresh session would hit again -> propose the exact fact to the user; only after approval `factsheet add \"fact\" -t tag`.
fix: fact seen wrong -> propose the change; only after approval `factsheet edit <id>` / `factsheet drop <id>`.
never add, edit or drop a fact without explicit user approval.
read: `factsheet ls` full view with tags and dates | full guide: `factsheet agent`.";

// Word overlap at or above this ratio marks two facts as near-duplicates.
const SIMILAR: f64 = 0.5;

fn main() {
    if let Err(e) = run() {
        eprintln!("factsheet: {e:#}");
        exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    config();
    let root = find_root();
    let store = root.join(".factsheet").join("facts.jsonl");

    match cli.cmd {
        None => inject(&store),
        Some(Cmd::Add { text, check, tags }) => add(&root, &store, text, check, &tags),
        Some(Cmd::Edit {
            id,
            text,
            check,
            tags,
        }) => edit(&store, &id, text, check, tags),
        Some(Cmd::Drop { id }) => drop_fact(&store, &id),
        Some(Cmd::Ls { tag, stale }) => ls(&store, &tag, stale),
        Some(Cmd::Tags) => tags(&store),
        Some(Cmd::Find { query }) => find(&store, &query.join(" ")),
        Some(Cmd::Dedup) => dedup(&store),
        Some(Cmd::Curate) => curate(&store),
        Some(Cmd::Projects) => projects(),
        Some(Cmd::Agent) => emit(AGENT_GUIDE.as_bytes()),
    }
}

// ---- config ----

struct Config {
    max_text: usize,
    hot_warn: usize,
    user_instructions: Result<String, &'static str>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_text: 200,
            hot_warn: 40,
            user_instructions: Ok(String::new()),
        }
    }
}

const DEFAULT_CONFIG: &str = "\
# factsheet config
# max fact text length in chars; longer facts are rejected
max_text = 200
# inject warns to curate when hot facts exceed this count
hot_warn = 40
";

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))?;
    Some(base.join("factsheet").join("config.toml"))
}

fn config() -> &'static Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    CONFIG.get_or_init(load_config)
}

fn load_config() -> Config {
    let mut cfg = Config::default();
    let Some(path) = config_path() else {
        return cfg;
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            if let Err(e) = create_config(&path) {
                eprintln!("factsheet: cannot create {}: {e}", path.display());
                cfg.user_instructions = Err("cannot create config; check file permissions");
            }
            return cfg;
        }
        Err(e) => {
            eprintln!("factsheet: cannot read {}: {e}", path.display());
            cfg.user_instructions =
                Err("cannot read config; check UTF-8 encoding and file permissions");
            return cfg;
        }
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        cfg.user_instructions = Err("invalid TOML syntax; config defaults used");
        return cfg;
    };
    for (key, value) in table {
        match key.as_str() {
            "user_instructions" => {
                cfg.user_instructions = value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("user_instructions must be a TOML string");
            }
            "max_text" => cfg.max_text = count_or(&key, &value, cfg.max_text),
            "hot_warn" => cfg.hot_warn = count_or(&key, &value, cfg.hot_warn),
            _ => eprintln!("factsheet: config: unknown key '{key}' ignored"),
        }
    }
    cfg
}

fn count_or(key: &str, value: &toml::Value, default: usize) -> usize {
    match value.as_integer().and_then(|n| usize::try_from(n).ok()) {
        Some(n) => n,
        None => {
            eprintln!("factsheet: config: '{key}' wants a non-negative integer; default kept");
            default
        }
    }
}

fn create_config(path: &Path) -> Result<()> {
    fs::create_dir_all(parent_dir(path)?)?;
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    file.write_all(DEFAULT_CONFIG.as_bytes())?;
    Ok(file.sync_all()?)
}

// ---- commands ----

fn inject(store: &Path) -> Result<()> {
    let facts = load(store)?;
    if store.exists() {
        register(store)?;
    }
    if facts.is_empty() {
        say("## Project memory (empty)")?;
        say(INJECT_RULES)?;
        return say_user_instructions();
    }
    // Reference facts stay out of the session context.
    let (hot, refs): (Vec<&Fact>, Vec<&Fact>) = facts.iter().partition(|f| !f.is_ref());
    if refs.is_empty() {
        say(&format!("## Project memory ({})", facts.len()))?;
    } else {
        say(&format!(
            "## Project memory ({} + {} ref)",
            hot.len(),
            refs.len()
        ))?;
    }
    for f in &hot {
        say(&render_bare(f))?;
    }
    say("---")?;
    say(INJECT_RULES)?;
    if !refs.is_empty() {
        say(&ref_footer(&refs))?;
    }
    let warn = config().hot_warn;
    if hot.len() > warn {
        say(&format!(
            "hot: {} > {warn} -> curate: merge dups, drop dead, demote lookups to ref",
            hot.len()
        ))?;
    }
    say_user_instructions()
}

fn ref_footer(refs: &[&Fact]) -> String {
    let mut counts = tag_counts(refs.iter().copied());
    counts.remove("ref");
    if counts.is_empty() {
        return format!("ref: {} facts, pull with `factsheet ls -t ref`", refs.len());
    }
    let breakdown: Vec<String> = counts.iter().map(|(t, n)| format!("#{t} {n}")).collect();
    format!(
        "ref: pull with `factsheet ls -t <tag>` | {}",
        breakdown.join(" ")
    )
}

// Hooks need config errors on stdout so the agent can report them.
fn say_user_instructions() -> Result<()> {
    match &config().user_instructions {
        Ok(s) if !s.is_empty() => {
            say("---")?;
            say(s)
        }
        Ok(_) => Ok(()),
        Err(why) => {
            say("---")?;
            say(&format!(
                "factsheet: config.toml: {why}. User instructions were not injected. \
Tell the user to check the global factsheet/config.toml: use user_instructions = \"text\" \
or triple quotes for multiple lines; escape double quotes and backslashes in basic strings, \
or use literal single-quoted strings."
            ))
        }
    }
}

// The text limit keeps each fact focused on one rule.
fn check_len(text: &str) -> Result<()> {
    let max = config().max_text;
    let n = text.chars().count();
    ensure!(
        n <= max,
        "fact too long ({n} > {max} chars) -> not stored. One rule per fact: \
split into atomic facts, prefix with topic (`azure db: ...`), move commands to -c"
    );
    Ok(())
}

fn add(
    root: &Path,
    store: &Path,
    text: Option<String>,
    check: Option<String>,
    tags: &[String],
) -> Result<()> {
    let text = text.unwrap_or_default();
    let check = check.unwrap_or_default();
    ensure!(
        !(text.trim().is_empty() && check.trim().is_empty()),
        "nothing to store: give fact text and/or --check <cmd>"
    );
    check_len(text.trim())?;
    ensure_dir(root, store)?;
    let _lock = lock_store(store)?;
    let facts = load(store)?;
    let fact = Fact {
        id: new_id(&facts),
        text: one_line(&text),
        check: one_line(&check),
        tags: clean_tags(tags),
        ts: today(),
    };
    commit(store, append(store, &fact))?;
    say(&render(&fact))?;
    // Similar facts are hints, not reasons to reject a write.
    let similar = rank_by_overlap(&facts, &fact.words());
    for (_, f) in similar.iter().take_while(|(s, _)| *s >= SIMILAR).take(3) {
        say(&format!("similar: {}", render(f)))?;
    }
    Ok(())
}

fn edit(
    store: &Path,
    id: &str,
    text: Option<String>,
    check: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<()> {
    ensure!(
        text.is_some() || check.is_some() || tags.is_some(),
        "nothing to change: give new text, --check and/or --tags"
    );
    ensure!(store.is_file(), "no fact with id '{id}'");
    let _lock = lock_store(store)?;
    let mut facts = load(store)?;
    let Some(f) = facts.iter_mut().find(|f| f.id == id) else {
        bail!("no fact with id '{id}'");
    };
    let content_changed = text.is_some() || check.is_some();
    if let Some(t) = text {
        check_len(t.trim())?;
        f.text = one_line(&t);
    }
    if let Some(c) = check {
        f.check = one_line(&c);
    }
    if let Some(t) = tags {
        f.tags = clean_tags(&t);
    }
    ensure!(
        !(f.text.is_empty() && f.check.is_empty()),
        "edit would leave the fact empty; use `factsheet drop` instead"
    );
    // Retagging does not verify the fact.
    if content_changed {
        f.ts = today();
    }
    let line = render(f);
    commit(store, save(store, &facts))?;
    say(&line)
}

fn drop_fact(store: &Path, id: &str) -> Result<()> {
    ensure!(store.is_file(), "no fact with id '{id}'");
    let _lock = lock_store(store)?;
    let mut facts = load(store)?;
    let before = facts.len();
    facts.retain(|f| f.id != id);
    ensure!(facts.len() < before, "no fact with id '{id}'");
    commit(store, save(store, &facts))?;
    say(&format!("dropped {id}"))
}

fn ls(store: &Path, tag: &[String], stale: Option<String>) -> Result<()> {
    let wanted = clean_tags(tag);
    let min_age = stale.map(|s| parse_days(&s)).transpose()?;
    let now = days_today();
    for f in &load(store)? {
        if !wanted.is_empty() && !f.tags.iter().any(|x| wanted.contains(x)) {
            continue;
        }
        if min_age.is_some_and(|n| now - days_from_ymd(&f.ts) < n) {
            continue;
        }
        say(&render(f))?;
    }
    Ok(())
}

fn tags(store: &Path) -> Result<()> {
    for (t, n) in tag_counts(&load(store)?) {
        say(&format!("#{t} {n}"))?;
    }
    Ok(())
}

fn tag_counts<'a>(facts: impl IntoIterator<Item = &'a Fact>) -> BTreeMap<&'a str, usize> {
    let mut counts = BTreeMap::new();
    for tag in facts.into_iter().flat_map(|f| &f.tags) {
        *counts.entry(tag.as_str()).or_insert(0) += 1;
    }
    counts
}

fn find(store: &Path, query: &str) -> Result<()> {
    let q = tokenize(query);
    ensure!(
        !q.is_empty(),
        "empty query: give at least one word of 2+ characters"
    );
    let facts = load(store)?;
    let hits = rank_by_overlap(&facts, &q);
    if hits.is_empty() {
        return say("no matches");
    }
    for (s, f) in hits.iter().take(5) {
        say(&format!("{s:.2} {}", render(f)))?;
    }
    Ok(())
}

// Lists only: merging facts needs a judgment call.
fn dedup(store: &Path) -> Result<()> {
    let facts = load(store)?;
    let max = config().max_text;
    let mut long: Vec<&Fact> = facts.iter().filter(|f| f.text_len() > max).collect();
    if !long.is_empty() {
        long.sort_by_key(|f| Reverse(f.text_len()));
        say(&format!(
            "over {max} chars ({}) -> split each into atomic facts (`factsheet add` per rule, then `factsheet drop` the old id):",
            long.len()
        ))?;
        for f in &long {
            say(&format!("  {} chars {}", f.text_len(), render(f)))?;
        }
    }
    let sets: Vec<Words> = facts.iter().map(Fact::words).collect();
    let mut pairs: Vec<(f64, usize, usize)> = (0..facts.len())
        .flat_map(|i| (i + 1..facts.len()).map(move |j| (i, j)))
        .map(|(i, j)| (overlap(&sets[i], &sets[j]), i, j))
        .filter(|(s, _, _)| *s >= SIMILAR)
        .collect();
    if pairs.is_empty() {
        return say(&format!("no near-duplicates (overlap >= {SIMILAR})"));
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (s, i, j) in pairs {
        say(&format!("{s:.2}"))?;
        say(&format!("  {}", render(&facts[i])))?;
        say(&format!("  {}", render(&facts[j])))?;
    }
    Ok(())
}

// ---- curate: edit the whole store as text ----

const CURATE_HELP: &str = "\
# factsheet curate. One fact per line: [id] text | check: cmd  #tag #tag  (date)
# delete a line = drop | change text, check or tags = edit | new line without [id] = add
# lines starting with # and the (date) are ignored | empty sheet = abort";

struct SheetLine {
    id: Option<String>,
    text: String,
    check: String,
    tags: Vec<String>,
}

#[derive(Default)]
struct Summary {
    added: usize,
    edited: usize,
    dropped: usize,
}

fn curate(store: &Path) -> Result<()> {
    ensure!(store.is_file(), "no facts to curate");
    let _lock = lock_store(store)?;
    let facts = load(store)?;
    let sheet = store.with_file_name("curate.txt");
    fs::write(&sheet, render_sheet(&facts))
        .with_context(|| format!("cannot write {}", sheet.display()))?;
    open_editor(&sheet)?;
    let edited =
        fs::read_to_string(&sheet).with_context(|| format!("cannot read {}", sheet.display()))?;
    let (facts, summary) = apply_sheet(facts, &edited)
        .with_context(|| format!("nothing applied, sheet kept at {}", sheet.display()))?;
    if let Err(e) = fs::remove_file(&sheet) {
        eprintln!("factsheet: cannot remove {}: {e}", sheet.display());
    }
    if summary.added + summary.edited + summary.dropped == 0 {
        return say("no changes");
    }
    commit(store, save(store, &facts))?;
    say(&format!(
        "added {}, edited {}, dropped {}",
        summary.added, summary.edited, summary.dropped
    ))
}

// Hot facts first, then ref; each fact once, under its first tag.
fn render_sheet(facts: &[Fact]) -> String {
    let mut groups: BTreeMap<(bool, &str), Vec<&Fact>> = BTreeMap::new();
    for f in facts {
        groups
            .entry((f.is_ref(), group_tag(f)))
            .or_default()
            .push(f);
    }
    let mut out = format!("{CURATE_HELP}\n");
    for ((is_ref, tag), group) in groups {
        let header = match (is_ref, tag) {
            (true, "") => "# ref".to_owned(),
            (true, tag) => format!("# ref: {tag}"),
            (false, tag) => format!("# {tag}"),
        };
        out.push_str(&format!("\n{header}\n"));
        for f in group {
            out.push_str(&format!("{}\n", render(f)));
        }
    }
    out
}

fn group_tag(f: &Fact) -> &str {
    match f.tags.iter().find(|t| *t != "ref") {
        Some(tag) => tag,
        None if f.is_ref() => "",
        None => "(untagged)",
    }
}

fn open_editor(path: &Path) -> Result<()> {
    let fallback = if cfg!(windows) { "notepad" } else { "vi" };
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned());
    let mut words = editor.split_whitespace();
    let program = words.next().context("empty editor command")?;
    let status = std::process::Command::new(program)
        .args(words)
        .arg(path)
        .status()
        .with_context(|| format!("cannot run editor '{editor}'"))?;
    ensure!(status.success(), "editor '{editor}' failed");
    Ok(())
}

fn apply_sheet(facts: Vec<Fact>, sheet: &str) -> Result<(Vec<Fact>, Summary)> {
    let lines = sheet
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(parse_sheet_line)
        .collect::<Result<Vec<SheetLine>>>()?;
    ensure!(!lines.is_empty(), "empty sheet");
    let today = today();
    let mut by_id: BTreeMap<String, Fact> = facts.into_iter().map(|f| (f.id.clone(), f)).collect();
    let mut summary = Summary::default();
    let mut kept = Vec::new();
    let mut fresh = Vec::new();
    for line in lines {
        ensure!(
            !(line.text.is_empty() && line.check.is_empty()),
            "empty fact: {}",
            line.id.as_deref().unwrap_or("(new)")
        );
        let Some(id) = line.id else {
            check_len(&line.text)?;
            fresh.push(line);
            continue;
        };
        let mut f = by_id
            .remove(&id)
            .with_context(|| format!("unknown or repeated id '{id}'"))?;
        let content_changed = f.text != line.text || f.check != line.check;
        if content_changed {
            check_len(&line.text)?;
            f.ts.clone_from(&today);
        }
        if content_changed || f.tags != line.tags {
            summary.edited += 1;
        }
        f.text = line.text;
        f.check = line.check;
        f.tags = line.tags;
        kept.push(f);
    }
    summary.dropped = by_id.len();
    summary.added = fresh.len();
    for line in fresh {
        kept.push(Fact {
            id: new_id(&kept),
            text: line.text,
            check: line.check,
            tags: line.tags,
            ts: today.clone(),
        });
    }
    Ok((kept, summary))
}

// Fields are separated by two spaces; one_line() keeps them out of text and check.
fn parse_sheet_line(line: &str) -> Result<SheetLine> {
    let mut parts = line.split("  ").map(str::trim).filter(|p| !p.is_empty());
    let head = parts
        .next()
        .with_context(|| format!("empty line: {line}"))?;
    let mut tags = Vec::new();
    for part in parts {
        if let Some(rest) = part.strip_prefix('#') {
            tags.extend(rest.split(" #").map(str::to_owned));
        } else if !(part.starts_with('(') && part.ends_with(')')) {
            bail!("cannot parse: {line}");
        }
    }
    let (id, body) = match head.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some((id, body)) => (Some(id.to_owned()), body.trim()),
        None => (None, head),
    };
    let (text, check) = match body.strip_prefix("check: ") {
        Some(check) => ("", check),
        None => body.split_once(" | check: ").unwrap_or((body, "")),
    };
    Ok(SheetLine {
        id,
        text: one_line(text),
        check: one_line(check),
        tags: clean_tags(&tags),
    })
}

// ---- similarity: lexical word overlap, no deps ----

fn tokenize(s: &str) -> Words {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(str::to_string)
        .collect()
}

// The overlap coefficient lets short queries match long facts.
fn overlap(a: &Words, b: &Words) -> f64 {
    let min = a.len().min(b.len());
    if min == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / min as f64
}

fn rank_by_overlap<'a>(facts: &'a [Fact], query: &Words) -> Vec<(f64, &'a Fact)> {
    let mut hits: Vec<(f64, &Fact)> = facts
        .iter()
        .map(|f| (overlap(query, &f.words()), f))
        .filter(|(s, _)| *s > 0.0)
        .collect();
    hits.sort_by(|a, b| b.0.total_cmp(&a.0));
    hits
}

// ---- store ----

// An existing store anchors projects without Git.
fn find_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.ancestors()
        .find(|dir| dir.join(".factsheet").is_dir() || dir.join(".git").exists())
        .unwrap_or(&cwd)
        .to_path_buf()
}

fn load(store: &Path) -> Result<Vec<Fact>> {
    let data = match fs::read_to_string(store) {
        Ok(data) => data,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", store.display())),
    };
    Ok(data
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| parse_fact(store, l))
        .collect())
}

// One corrupt line must not hide the remaining facts.
fn parse_fact(store: &Path, line: &str) -> Option<Fact> {
    match serde_json::from_str::<Fact>(line) {
        Ok(mut f) => {
            f.tags = clean_tags(&f.tags);
            Some(f)
        }
        Err(e) => {
            eprintln!(
                "factsheet: skipping corrupt line in {}: {e}",
                store.display()
            );
            None
        }
    }
}

fn ensure_dir(root: &Path, store: &Path) -> Result<()> {
    let dir = parent_dir(store)?;
    if dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    add_git_exclude(root);
    Ok(())
}

// A separate lock file stays in place while the store is replaced.
fn lock_store(store: &Path) -> Result<File> {
    let path = store.with_file_name(".lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    fs2::FileExt::lock_exclusive(&file)
        .with_context(|| format!("cannot lock {}", path.display()))?;
    Ok(file)
}

fn commit(store: &Path, written: Result<()>) -> Result<()> {
    written.with_context(|| format!("cannot write {}", store.display()))?;
    register(store)
}

fn append(store: &Path, fact: &Fact) -> Result<()> {
    let json = serde_json::to_string(fact)?;
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(store)?;
    // A killed append may leave an unfinished last line.
    if f.metadata()?.len() > 0 {
        f.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        f.read_exact(&mut last)?;
        if last != *b"\n" {
            f.write_all(b"\n")?;
        }
    }
    writeln!(f, "{json}")?;
    Ok(f.sync_all()?)
}

fn save(store: &Path, facts: &[Fact]) -> Result<()> {
    let dir = parent_dir(store)?;
    // The backup must be on disk before the store is replaced.
    let backup = store.with_file_name("facts.jsonl.bak");
    fs::copy(store, &backup)?;
    OpenOptions::new().write(true).open(&backup)?.sync_all()?;
    let tmp = dir.join(".facts.jsonl.tmp");
    let mut file = File::create(&tmp)?;
    for fact in facts {
        serde_json::to_writer(&mut file, fact)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, store)?;
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}

fn add_git_exclude(root: &Path) {
    if !root.join(".git").exists() {
        return;
    }
    // Linked worktrees share the main repository's exclude file.
    let out = match std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
    {
        Ok(out) if out.status.success() => out,
        Ok(_) => {
            eprintln!("factsheet: cannot locate Git exclude file");
            return;
        }
        Err(e) => {
            eprintln!("factsheet: cannot locate Git exclude file: {e}");
            return;
        }
    };
    let git = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let exclude = git.join("info").join("exclude");
    let current = match fs::read_to_string(&exclude) {
        Ok(current) => current,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => {
            eprintln!("factsheet: cannot read {}: {e}", exclude.display());
            return;
        }
    };
    if current.lines().any(|l| l.trim() == "/.factsheet/") {
        return;
    }
    let mut updated = current;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str("/.factsheet/\n");
    if let Err(e) = fs::create_dir_all(git.join("info")).and_then(|()| fs::write(&exclude, updated))
    {
        eprintln!("factsheet: cannot write {}: {e}", exclude.display());
    }
}

// ---- registry ----

fn registry_path() -> Option<PathBuf> {
    Some(config_path()?.with_file_name("projects"))
}

fn load_registry() -> Result<Vec<PathBuf>> {
    let Some(path) = registry_path() else {
        return Ok(Vec::new());
    };
    match fs::read_to_string(&path) {
        Ok(text) => Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(PathBuf::from)
            .collect()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn save_registry(paths: &[PathBuf]) {
    let Some(path) = registry_path() else { return };
    let out: String = paths.iter().map(|p| format!("{}\n", p.display())).collect();
    if let Err(e) = fs::write(&path, out) {
        eprintln!("factsheet: cannot write {}: {e}", path.display());
    }
}

fn register(store: &Path) -> Result<()> {
    let store = fs::canonicalize(store).unwrap_or_else(|_| store.to_path_buf());
    let mut known = load_registry()?;
    if !known.contains(&store) {
        known.push(store);
        save_registry(&known);
    }
    Ok(())
}

// Missing stores are pruned on every listing.
fn projects() -> Result<()> {
    let known = load_registry()?;
    let (alive, gone): (Vec<PathBuf>, Vec<PathBuf>) = known.into_iter().partition(|p| p.is_file());
    for store in &alive {
        let facts = load(store)?;
        let last = facts.iter().map(|f| f.ts.as_str()).max().unwrap_or("-");
        let root = store.parent().and_then(Path::parent).unwrap_or(store);
        say(&format!(
            "{}  {} facts  last {last}",
            root.display(),
            facts.len()
        ))?;
    }
    for store in &gone {
        say(&format!(
            "gone: {} (removed from registry)",
            store.display()
        ))?;
    }
    if !gone.is_empty() {
        save_registry(&alive);
    }
    if alive.is_empty() && gone.is_empty() {
        say("no projects registered yet; the registry fills as factsheet runs in each project")?;
    }
    Ok(())
}

// ---- helpers ----

fn parent_dir(path: &Path) -> Result<&Path> {
    path.parent()
        .with_context(|| format!("no parent directory: {}", path.display()))
}

// Tags and dates stay out of the session context to save tokens.
fn render_bare(f: &Fact) -> String {
    let body = match (f.text.is_empty(), f.check.is_empty()) {
        (false, false) => format!("{} | check: {}", f.text, f.check),
        (true, false) => format!("check: {}", f.check),
        _ => f.text.clone(),
    };
    format!("[{}] {}", f.id, body)
}

fn render(f: &Fact) -> String {
    let tags = if f.tags.is_empty() {
        String::new()
    } else {
        format!("  #{}", f.tags.join(" #"))
    };
    format!("{}{}  ({})", render_bare(f), tags, f.ts)
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    tags.iter()
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty() && seen.insert(*tag))
        .map(str::to_owned)
        .collect()
}

fn new_id(existing: &[Fact]) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let nanos = unix_now().as_nanos() as u64;
    let mut state = (nanos ^ (u64::from(std::process::id()) << 32)) | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut len = 3;
    let mut tries = 0;
    loop {
        let id: String = (0..len)
            .map(|_| CHARS[(next() % CHARS.len() as u64) as usize] as char)
            .collect();
        if !existing.iter().any(|f| f.id == id) {
            return id;
        }
        tries += 1;
        if tries > 100 {
            len += 1;
            tries = 0;
        }
    }
}

fn parse_days(s: &str) -> Result<i64> {
    match s.trim().trim_end_matches('d').parse::<i64>() {
        Ok(n) if n >= 0 => Ok(n),
        _ => bail!("bad --stale value '{s}', expected e.g. 30d"),
    }
}

fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

fn days_today() -> i64 {
    i64::try_from(unix_now().as_secs() / 86400).unwrap_or(0)
}

fn today() -> String {
    let (y, m, d) = civil_from_days(days_today());
    format!("{y:04}-{m:02}-{d:02}")
}

// Unreadable dates count as stale.
fn days_from_ymd(s: &str) -> i64 {
    let parts: Vec<i64> = s.split('-').filter_map(|p| p.parse().ok()).collect();
    let [y, m, d] = parts[..] else {
        return 0;
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return 0;
    }
    days_from_civil(y, m, d)
}

// Howard Hinnant's civil date algorithms.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn say(s: &str) -> Result<()> {
    emit(format!("{s}\n").as_bytes())
}

fn emit(bytes: &[u8]) -> Result<()> {
    write_output(&mut io::stdout(), bytes).context("cannot write stdout")
}

fn write_output(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    match writer.write_all(bytes) {
        // Piping into head is not an error.
        Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, days_from_civil, write_output};
    use std::io::{self, Write};

    struct FailedOutput(io::ErrorKind);

    impl Write for FailedOutput {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn ignores_only_broken_pipe_errors() {
        assert!(write_output(&mut FailedOutput(io::ErrorKind::BrokenPipe), b"text").is_ok());
        let error = write_output(&mut FailedOutput(io::ErrorKind::Other), b"text")
            .expect_err("non-pipe output failures must propagate");
        assert_eq!(error.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn civil_dates_round_trip() {
        for days in [0, 19_970, 20_700, 25_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }
}

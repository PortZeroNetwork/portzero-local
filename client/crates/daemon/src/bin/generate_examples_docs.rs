//! Regenerates `installer/getting-started.json` and `docs/examples.md` from
//! the adjacent `portzero-examples` checkout. Run via `just examples-docs`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;

const REPO_URL: &str = "https://github.com/PortZeroNetwork/portzero-examples.git";

const LANGUAGES: &[Language] = &[
    Language::new("typescript", "TypeScript", &["node", "nodejs", "npm"]),
    Language::new("javascript", "JavaScript", &["node", "nodejs"]),
    Language::new("python", "Python", &["python3", "python"]),
    Language::new("java", "Java", &["java", "javac"]),
    Language::new("go", "Go", &["go"]),
    Language::new("rust", "Rust", &["cargo", "rustc"]),
    Language::new("csharp", "C#", &["dotnet"]),
    Language::new("php", "PHP", &["php", "composer"]),
    Language::new("ruby", "Ruby", &["ruby", "bundle"]),
    Language::new("cpp", "C/C++", &["cc", "gcc", "clang", "g++", "clang++"]),
    Language::new("swift", "Swift", &["swift"]),
    Language::new("kotlin", "Kotlin", &["kotlin", "kotlinc"]),
    Language::new("dart", "Dart", &["dart"]),
    Language::new("elixir", "Elixir", &["elixir", "mix"]),
    Language::new("scala", "Scala", &["scala", "sbt"]),
    Language::new("r", "R", &["R", "Rscript"]),
    Language::new("julia", "Julia", &["julia"]),
    Language::new("lua", "Lua", &["lua", "luajit"]),
    Language::new("perl", "Perl", &["perl"]),
    Language::new("zig", "Zig", &["zig"]),
    Language::new("haskell", "Haskell", &["ghc", "cabal", "stack"]),
    Language::new("shell", "Shell", &["bash", "sh"]),
];

#[derive(Clone, Copy, Serialize)]
struct Language {
    id: &'static str,
    label: &'static str,
    tools: &'static [&'static str],
}

impl Language {
    const fn new(id: &'static str, label: &'static str, tools: &'static [&'static str]) -> Self {
        Self { id, label, tools }
    }
}

#[derive(Clone)]
struct Example {
    language: String,
    variant: String,
    path: PathBuf,
}

#[derive(Serialize)]
struct Manifest {
    source: Source,
    languages: Vec<Language>,
    examples: Vec<ExampleRecord>,
}

#[derive(Serialize)]
struct Source {
    repo: &'static str,
    path: &'static str,
    commit: Option<String>,
}

#[derive(Serialize)]
struct ExampleRecord {
    id: String,
    title: String,
    language: String,
    language_label: String,
    tools: Vec<&'static str>,
    variant: String,
    variant_label: String,
    requires_docker: bool,
    path: String,
    domain: String,
    scripts: PlatformMap,
    commands: PlatformMap,
}

#[derive(Serialize)]
struct PlatformMap {
    linux: String,
    macos: String,
    windows: String,
}

fn main() -> Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .context("resolve repository root")?;
    let examples_root = repo_root
        .join("../portzero-examples")
        .canonicalize()
        .context(
            "resolve ../portzero-examples; clone it next to portzero-local before running this",
        )?;

    let examples = discover_examples(&examples_root)?;
    anyhow::ensure!(
        !examples.is_empty(),
        "no examples found in {}",
        examples_root.display()
    );

    let manifest = Manifest {
        source: Source {
            repo: REPO_URL,
            path: "../portzero-examples",
            commit: git_commit(&examples_root),
        },
        languages: LANGUAGES.to_vec(),
        examples: examples.iter().map(example_record).collect(),
    };

    let manifest_path = repo_root.join("installer/getting-started.json");
    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(&manifest_path, format!("{manifest_json}\n"))
        .with_context(|| format!("write {}", manifest_path.display()))?;
    println!("wrote {}", manifest_path.display());

    let docs_path = repo_root.join("docs/examples.md");
    std::fs::write(&docs_path, render_docs(&examples))
        .with_context(|| format!("write {}", docs_path.display()))?;
    println!("wrote {}", docs_path.display());

    Ok(())
}

fn discover_examples(root: &Path) -> Result<Vec<Example>> {
    let mut language_dirs = std::fs::read_dir(root)
        .with_context(|| format!("read {}", root.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    language_dirs.sort_by_key(|entry| language_rank(&entry.file_name().to_string_lossy()));

    let mut examples = Vec::new();
    for language_entry in language_dirs {
        let language_path = language_entry.path();
        let language = language_entry.file_name().to_string_lossy().into_owned();
        if !language_path.is_dir()
            || language.starts_with('.')
            || matches!(language.as_str(), "scripts" | "target")
        {
            continue;
        }

        let mut variant_dirs = std::fs::read_dir(&language_path)
            .with_context(|| format!("read {}", language_path.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        variant_dirs.sort_by_key(|entry| variant_rank(&entry.file_name().to_string_lossy()));

        for variant_entry in variant_dirs {
            let variant_path = variant_entry.path();
            let variant = variant_entry.file_name().to_string_lossy().into_owned();
            if !variant_path.is_dir() || variant.starts_with('.') || !has_runner(&variant_path) {
                continue;
            }
            examples.push(Example {
                language: language.clone(),
                variant,
                path: variant_path
                    .strip_prefix(root)
                    .unwrap_or(&variant_path)
                    .to_path_buf(),
            });
        }
    }

    Ok(examples)
}

fn language_rank(language: &str) -> (usize, String) {
    let rank = LANGUAGES
        .iter()
        .position(|item| item.id == language)
        .unwrap_or(LANGUAGES.len());
    (rank, language.to_string())
}

fn variant_rank(variant: &str) -> (usize, String) {
    let rank = match variant {
        "process" => 0,
        "docker" => 1,
        _ => 2,
    };
    (rank, variant.to_string())
}

fn has_runner(path: &Path) -> bool {
    let scripts = path.join("scripts");
    scripts.join("run.sh").is_file() || scripts.join("run.ps1").is_file()
}

fn example_record(example: &Example) -> ExampleRecord {
    let path = slash_path(&example.path);
    let domain = domain(example);
    let unix_command = format!("PZ_TUNNEL=\"{domain}\" ./scripts/run.sh");
    let windows_command = format!("$env:PZ_TUNNEL = \"{domain}\"; .\\scripts\\run.ps1");
    let language = language(example.language.as_str());

    ExampleRecord {
        id: format!("{}/{}", example.language, example.variant),
        title: title(example),
        language: example.language.clone(),
        language_label: language
            .map(|item| item.label)
            .unwrap_or(example.language.as_str())
            .to_string(),
        tools: language.map(|item| item.tools.to_vec()).unwrap_or_default(),
        variant: example.variant.clone(),
        variant_label: variant_label(&example.variant).to_string(),
        requires_docker: example.variant == "docker",
        path: path.clone(),
        domain: domain.clone(),
        scripts: PlatformMap {
            linux: format!("{path}/scripts/run.sh"),
            macos: format!("{path}/scripts/run.sh"),
            windows: format!("{path}/scripts/run.ps1"),
        },
        commands: PlatformMap {
            linux: unix_command.clone(),
            macos: unix_command,
            windows: windows_command,
        },
    }
}

fn language(id: &str) -> Option<Language> {
    LANGUAGES.iter().copied().find(|item| item.id == id)
}

fn title(example: &Example) -> String {
    let language = language(example.language.as_str())
        .map(|item| item.label)
        .unwrap_or(example.language.as_str());
    format!("{language} {}", variant_label(&example.variant))
}

fn variant_label(variant: &str) -> &str {
    match variant {
        "process" => "process",
        "docker" => "Docker container",
        _ => variant,
    }
}

fn domain(example: &Example) -> String {
    format!("{}-{}.portzero.local:80", example.language, example.variant)
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn git_commit(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

fn render_docs(examples: &[Example]) -> String {
    let mut lines = vec![
        "# PortZero Examples".to_string(),
        String::new(),
        "These examples live in the separate `portzero-examples` repository. Clone it next to this repository or anywhere convenient:".to_string(),
        String::new(),
        "```sh".to_string(),
        format!("git clone {REPO_URL}"),
        "cd portzero-examples".to_string(),
        "```".to_string(),
        String::new(),
        "Each example sets `PZ_TUNNEL` so the local daemon can make the process or Docker container available at a `*.portzero.local` name.".to_string(),
        String::new(),
    ];

    for example in examples {
        let path = slash_path(&example.path);
        let windows_path = path.replace('/', "\\");
        let domain = domain(example);
        lines.extend([
            format!("## {}", title(example)),
            String::new(),
            "macOS or Linux:".to_string(),
            String::new(),
            "```sh".to_string(),
            format!("cd {path}"),
            format!("PZ_TUNNEL=\"{domain}\" ./scripts/run.sh"),
            "```".to_string(),
            String::new(),
            "Windows PowerShell:".to_string(),
            String::new(),
            "```powershell".to_string(),
            format!("cd {windows_path}"),
            format!("$env:PZ_TUNNEL = \"{domain}\""),
            ".\\scripts\\run.ps1".to_string(),
            "```".to_string(),
            String::new(),
        ]);

        if example.variant == "docker" {
            lines.extend([
                "This example requires Docker to be installed and running.".to_string(),
                String::new(),
            ]);
        }

        lines.extend([
            format!(
                "Open `http://{}/` after the example starts.",
                domain.trim_end_matches(":80")
            ),
            String::new(),
        ]);
    }

    lines.join("\n")
}

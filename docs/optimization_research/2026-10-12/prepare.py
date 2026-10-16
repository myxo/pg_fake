from pathlib import Path
import json
import re
import subprocess
import sys


def prepare_probes():
    evidence = Path(__file__).resolve().parent
    repository = evidence.parents[2]
    output = repository / "target" / "optimization-research"
    output.mkdir(parents=True, exist_ok=True)
    if not any(arg in sys.argv for arg in ("--e01", "--e02", "--e02-regression", "--e03", "--e03-regression", "--e03-final-regression", "--e04", "--e05", "--e05-controls", "--e05-concurrency", "--e05-maintenance", "--e05-final-controls", "--e05-indexed-write", "--e07", "--e08", "--full-fake", "--regression", "--serializable")):
        build = subprocess.run(
            ["cargo", "build", "--offline", "--release", "-p", "pg_fake", "--message-format=json"],
            cwd=repository, capture_output=True, text=True, check=True,
        )
        libraries = [
            filename
            for line in build.stdout.splitlines()
            if (message := json.loads(line)).get("reason") == "compiler-artifact"
            and message["target"]["name"] == "pg_fake"
            for filename in message["filenames"]
            if filename.endswith(".rlib")
        ]
        assert len(libraries) == 1
        for name in ["probe", "handoff_probe"]:
            subprocess.run(
                ["rustc", "--edition=2024", "-O", "-g", str(evidence / f"{name}.rs"),
                 "--extern", f"pg_fake={libraries[0]}", "-L",
                 f"dependency={repository / 'target/release/deps'}", "-o", str(output / name)],
                check=True,
            )
    if "--sqlx" not in sys.argv:
        print(output / "probe")
        return

    source = (repository / "crates/pg_fake_benchmarks/benches/workloads.rs").read_text()
    start = source.index("fn benchmarks(criterion: &mut Criterion) {")
    end = source.index("\nfn on_conflict_benchmark(", start)
    entry = (
        "fake_e01_entry.rs" if "--e01" in sys.argv else
        "fake_e02_regression_entry.rs" if "--e02-regression" in sys.argv else
        "fake_e02_entry.rs" if "--e02" in sys.argv else
        "fake_e04_entry.rs" if "--e04" in sys.argv else
        "fake_e05_indexed_write_entry.rs" if "--e05-indexed-write" in sys.argv else
        "fake_e05_final_controls_entry.rs" if "--e05-final-controls" in sys.argv else
        "fake_e05_maintenance_entry.rs" if "--e05-maintenance" in sys.argv else
        "fake_e05_concurrency_entry.rs" if "--e05-concurrency" in sys.argv else
        "fake_e05_controls_entry.rs" if "--e05-controls" in sys.argv else
        "fake_e05_entry.rs" if "--e05" in sys.argv else
        "fake_e07_entry.rs" if "--e07" in sys.argv else
        "fake_e08_entry.rs" if "--e08" in sys.argv else
        "fake_e03_final_regression_entry.rs" if "--e03-final-regression" in sys.argv else
        "fake_e03_regression_entry.rs" if "--e03-regression" in sys.argv else
        "fake_e03_entry.rs" if "--e03" in sys.argv else
        "fake_regression_entry.rs" if "--regression" in sys.argv else
        "fake_serializable_entry.rs" if "--serializable" in sys.argv else
        "fake_entry.rs"
    )
    source = source[:start] + (evidence / entry).read_text() + source[end:]
    if "--use-cli-timing" in sys.argv:
        source, overrides = re.subn(
            r"(?m)^([ \t]*)group\.sample_size\(20\);\n\1group\.measurement_time\(Duration::from_secs\(2\)\);\n",
            "",
            source,
        )
        assert overrides == 3
    (output / "workloads.rs").write_text(source)
    manifest = (repository / "crates/pg_fake_benchmarks/Cargo.toml").read_text()
    manifest = manifest.replace('name = "pg_fake_benchmarks"', 'name = "optimization-research"', 1)
    manifest = manifest.replace('edition.workspace = true', 'edition = "2024"')
    manifest = manifest.replace('license.workspace = true', 'license = "MIT"')
    manifest = manifest.replace('[lints]\nworkspace = true\n', '[workspace]\n')
    manifest = manifest.replace('[dev-dependencies]', '[dependencies]')
    for name in ["pg_fake", "pg_fake_sqlx"]:
        manifest = manifest.replace(f'path = "../{name}"', f'path = "{repository}/crates/{name}"')
    manifest = manifest[:manifest.index('[[bench]]')]
    manifest += f'pg_fake_benchmarks = {{ path = "{repository}/crates/pg_fake_benchmarks" }}\n'
    manifest += '\n[[bin]]\nname = "research-workloads"\npath = "workloads.rs"\n'
    (output / "Cargo.toml").write_text(manifest)
    (output / "Cargo.lock").write_bytes((repository / "Cargo.lock").read_bytes())
    subprocess.run(
        ["cargo", "build", "--offline", "--release", "--manifest-path",
         str(output / "Cargo.toml"), "--target-dir", str(repository / "target")],
        check=True,
    )
    print(repository / "target/release/research-workloads")


if __name__ == "__main__":
    prepare_probes()

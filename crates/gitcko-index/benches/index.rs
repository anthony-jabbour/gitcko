// SPDX-License-Identifier: AGPL-3.0-or-later

//! Index throughput on a generated workspace of about 2,000 files, and the latency of an
//! incremental build after a one-file edit (TESTING L8). Run nightly, never gated.

use criterion::{Criterion, criterion_group, criterion_main};
use gitcko_git::Repo;
use gitcko_index::{IndexLimits, build, build_incremental};
use gitcko_test_support::{empty_repo, literal_commit};

const FILES: usize = 2_000;

/// Module `i` of the generated crate, calling into the previous one. `edit` changes its
/// body without changing anything else.
fn module(i: usize, edit: bool) -> String {
    let mut text = String::new();
    if i > 0 {
        text += &format!("use crate::m{}::f{};\n\n", i - 1, i - 1);
    }
    text += &format!("/// Step {i}.\npub fn f{i}(x: u32) -> u32 {{\n");
    if i > 0 {
        text += &format!("    let y = f{}(x);\n", i - 1);
    } else {
        text += "    let y = x;\n";
    }
    text += &format!("    y + {}\n}}\n", if edit { i + 1 } else { i });
    text += &format!(
        "\npub struct S{i} {{\n    pub value: u32,\n}}\n\nimpl S{i} {{\n    pub fn get(&self) -> u32 {{\n        f{i}(self.value)\n    }}\n}}\n"
    );
    text
}

fn workspace(edited: Option<usize>) -> Vec<(String, String)> {
    let modules = FILES - 2;
    let mut files = vec![
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"bench\"\nversion = \"0.1.0\"\n".to_string(),
        ),
        (
            "src/lib.rs".to_string(),
            (0..modules).map(|i| format!("mod m{i};\n")).collect(),
        ),
    ];
    for i in 0..modules {
        files.push((format!("src/m{i}.rs"), module(i, edited == Some(i))));
    }
    files
}

fn commit(dir: &std::path::Path, files: &[(String, String)]) -> String {
    let list: Vec<(&[u8], &str, &[u8])> = files
        .iter()
        .map(|(p, t)| (p.as_bytes(), "100644", t.as_bytes()))
        .collect();
    literal_commit(dir, &list)
}

fn bench(c: &mut Criterion) {
    let dir = empty_repo("bench");
    let before = commit(dir.path(), &workspace(None));
    let after = commit(dir.path(), &workspace(Some(FILES / 2)));
    let repo = Repo::open(dir.path()).unwrap();
    let before = repo.resolve(&before).unwrap();
    let after = repo.resolve(&after).unwrap();
    let limits = IndexLimits::default();
    let previous = build(&repo, before, &limits).unwrap();

    let mut group = c.benchmark_group("index");
    group.sample_size(10);
    group.throughput(criterion::Throughput::Elements(FILES as u64));
    group.bench_function("clean_build_2000_files", |b| {
        b.iter(|| build(&repo, before, &limits).unwrap())
    });
    group.bench_function("incremental_after_one_file_edit", |b| {
        b.iter(|| build_incremental(&repo, after, &limits, &previous).unwrap())
    });
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);

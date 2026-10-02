//! Trains and checks Petal's path classifier (what kind of thing a file or folder is,
//! from its path alone). The feature and model code is the app's own, in `src/classify`,
//! so what's trained here is exactly what Petal runs.
//!
//!     cargo run --release -- eval               # train, then report accuracy
//!     cargo run --release -- train              # train and write src/classify/model.bin
//!     cargo run --release -- classify <path>... # use the shipped model (`--file` for files)

#[path = "../../../src/classify/features.rs"]
mod features;
#[path = "../../../src/classify/model.rs"]
mod model;
mod synth;
mod testset;

use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use model::{CATEGORIES, Category, Model};
use synth::{Item, Split};

const TRAIN_SIZE: usize = 80_000;
const TEST_SIZE: usize = 10_000;
const MODEL_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../src/classify/model.bin");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("eval") => {
            let (model, train) = train();
            eval(&model, &train);
        }
        Some("train") => {
            let (model, _) = train();
            std::fs::write(MODEL_FILE, model.to_bytes()).expect("write model");
            println!("wrote {MODEL_FILE}");
        }
        Some("classify") => classify(&args[1..]),
        _ => eprintln!("usage: path-classifier eval | train | classify [--file] <path>..."),
    }
}

/// Trains on generated paths and returns the model as shipped: quantized to bytes and
/// read back.
fn train() -> (Model, Vec<Item>) {
    let started = Instant::now();
    let train = synth::generate(Split::Train, TRAIN_SIZE, 1);
    let data: Vec<_> = train.iter().map(|i| (features::extract(&i.path, i.is_file), i.category)).collect();
    let mut rng = synth::Rng::new(7);
    let trained = Model::train(&data, 4, 0.5, 1e-6, |order| rng.shuffle(order));
    let bytes = trained.to_bytes();
    println!("trained on {} generated paths in {:.1} s; model is {} KB", train.len(), started.elapsed().as_secs_f32(), bytes.len() / 1024);
    (Model::from_bytes(&bytes).unwrap(), train)
}

fn eval(model: &Model, train: &[Item]) {
    let generated = synth::generate(Split::Test, TEST_SIZE, 2);
    let cases: Vec<_> = generated.iter().map(|i| (i.path.as_str(), i.is_file, i.category)).collect();
    report("Generated paths, names unseen in training", model, &cases, false);

    let seen: HashSet<&str> = train.iter().map(|i| i.path.as_str()).collect();
    let hand: Vec<_> = testset::PATHS.to_vec();
    let verbatim = hand.iter().filter(|c| seen.contains(c.0)).count();
    report("Hand-labelled paths, all", model, &hand, true);
    let unseen: Vec<_> = hand.into_iter().filter(|c| !seen.contains(c.0)).collect();
    report(&format!("Hand-labelled paths never generated verbatim ({verbatim} of {} were)", testset::PATHS.len()), model, &unseen, true);

    let started = Instant::now();
    let rounds = 1_000;
    for _ in 0..rounds {
        for &(path, is_file, _) in testset::PATHS {
            std::hint::black_box(model.predict(&features::extract(path, is_file)));
        }
    }
    let per = started.elapsed().as_secs_f64() / (rounds * testset::PATHS.len()) as f64;
    println!("\nspeed: {:.1} µs per path, features included", per * 1e6);
}

fn report(title: &str, model: &Model, cases: &[(&str, bool, Category)], list_misses: bool) {
    let mut per_label: BTreeMap<Category, (usize, usize)> = BTreeMap::new();
    let mut misses = Vec::new();
    for &(path, is_file, label) in cases {
        let (guess, p) = model.predict(&features::extract(path, is_file));
        let entry = per_label.entry(label).or_default();
        entry.1 += 1;
        if guess == label {
            entry.0 += 1;
        } else {
            misses.push((label, guess, p[guess.ix()], path));
        }
    }
    let right = cases.len() - misses.len();
    println!("\n=== {title} ===");
    println!("accuracy {right}/{} = {:.1}%", cases.len(), 100.0 * right as f64 / cases.len() as f64);
    let line: Vec<String> = CATEGORIES
        .iter()
        .filter_map(|c| per_label.get(c).map(|(r, n)| format!("{} {r}/{n}", c.label())))
        .collect();
    println!("by kind: {}", line.join(" · "));
    if list_misses {
        misses.sort_by_key(|m| (m.0, m.1));
        for (label, guess, p, path) in misses {
            println!("  {:<22} -> {:<22} p={p:.2}  {path}", label.label(), guess.label());
        }
    }
}

fn classify(args: &[String]) {
    let bytes = std::fs::read(MODEL_FILE).unwrap_or_else(|_| panic!("no {MODEL_FILE}; run `train` first"));
    let model = Model::from_bytes(&bytes).unwrap();
    let is_file = args.iter().any(|a| a == "--file");
    for path in args.iter().filter(|a| !a.starts_with("--")) {
        let (category, p) = model.predict(&features::extract(path, is_file));
        println!("{:<22} p={:.2}  {path}", category.label(), p[category.ix()]);
    }
}

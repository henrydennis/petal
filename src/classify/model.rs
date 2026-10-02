//! Multinomial logistic regression over hashed path features. Trained offline by
//! `tools/path-classifier` (which shares this file) and shipped as one signed byte per
//! weight.

use super::features::{BUCKETS, BITS, VERSION};

/// What a file or folder is, judged from its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Apps,
    Downloads,
    Developer,
    Caches,
    Media,
    Documents,
    AppData,
    System,
    /// A folder holding a bit of everything, such as the home folder or ~/Library.
    Mixed,
}

pub const CATEGORIES: [Category; 9] = [
    Category::Apps,
    Category::Downloads,
    Category::Developer,
    Category::Caches,
    Category::Media,
    Category::Documents,
    Category::AppData,
    Category::System,
    Category::Mixed,
];
const K: usize = CATEGORIES.len();

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Apps => "Apps",
            Category::Downloads => "Downloads & Trash",
            Category::Developer => "Developer files",
            Category::Caches => "Caches & logs",
            Category::Media => "Photos, video & music",
            Category::Documents => "Documents",
            Category::AppData => "App data & backups",
            Category::System => "System",
            Category::Mixed => "Mixed folders",
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ix(self) -> usize {
        self as usize
    }
}

pub struct Model {
    /// `weights[feature * K + class]`
    weights: Vec<f32>,
}

const MAGIC: &[u8; 4] = b"PCL2";
const HEADER: usize = 4 + 2 + 1 + 1 + 4;

impl Model {
    pub fn predict(&self, features: &[u32]) -> (Category, [f32; K]) {
        let p = probabilities(&self.weights, features);
        let best = (0..K).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap();
        (CATEGORIES[best], p)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Model, String> {
        if bytes.len() != HEADER + BUCKETS * K
            || &bytes[..4] != MAGIC
            || u16::from_le_bytes([bytes[4], bytes[5]]) != VERSION
            || bytes[6] as u32 != BITS
            || bytes[7] as usize != K
        {
            return Err("model file doesn't match this feature layout; retrain it with tools/path-classifier".into());
        }
        let scale = f32::from_le_bytes(bytes[8..12].try_into().unwrap());
        Ok(Model { weights: bytes[HEADER..].iter().map(|&b| b as i8 as f32 * scale).collect() })
    }
}

/// Training and saving, used only by `tools/path-classifier`.
#[allow(dead_code)]
impl Model {
    pub fn train(data: &[(Vec<u32>, Category)], epochs: usize, rate: f32, l2: f32, mut shuffle: impl FnMut(&mut [usize])) -> Model {
        let mut weights = vec![0f32; BUCKETS * K];
        let mut grad_sq = vec![1e-6f32; BUCKETS * K];
        let mut order: Vec<usize> = (0..data.len()).collect();
        for _ in 0..epochs {
            shuffle(&mut order);
            for &i in &order {
                let (features, label) = &data[i];
                let p = probabilities(&weights, features);
                for k in 0..K {
                    let target = if k == label.ix() { 1.0 } else { 0.0 };
                    let g_out = p[k] - target;
                    for &f in features {
                        let j = f as usize * K + k;
                        let g = g_out + l2 * weights[j];
                        grad_sq[j] += g * g;
                        weights[j] -= rate * g / grad_sq[j].sqrt();
                    }
                }
            }
        }
        Model { weights }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let max = self.weights.iter().fold(0f32, |m, w| m.max(w.abs())).max(1e-6);
        let scale = max / 127.0;
        let mut out = Vec::with_capacity(HEADER + self.weights.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.push(BITS as u8);
        out.push(K as u8);
        out.extend_from_slice(&scale.to_le_bytes());
        out.extend(self.weights.iter().map(|w| (w / scale).round().clamp(-127.0, 127.0) as i8 as u8));
        out
    }
}

fn probabilities(weights: &[f32], features: &[u32]) -> [f32; K] {
    let mut z = [0f32; K];
    for &f in features {
        let row = &weights[f as usize * K..f as usize * K + K];
        for k in 0..K {
            z[k] += row[k];
        }
    }
    let m = z.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let e = z.map(|v| (v - m).exp());
    let s: f32 = e.iter().sum();
    e.map(|v| v / s)
}

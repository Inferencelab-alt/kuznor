pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cosine_known_values() {
        assert!((cosine_similarity(&[1., 0.], &[1., 0.]) - 1.).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[1., 0.], &[0., 1.]), 0.);
    }
}

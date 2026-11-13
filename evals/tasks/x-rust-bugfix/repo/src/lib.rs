/// Merge overlapping or touching inclusive ranges. Input need not be sorted.
pub fn merge(mut ranges: Vec<(i32, i32)>) -> Vec<(i32, i32)> {
    ranges.sort();
    let mut out: Vec<(i32, i32)> = Vec::new();
    for (start, end) in ranges {
        match out.last_mut() {
            Some(last) if start < last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping() {
        assert_eq!(merge(vec![(1, 5), (3, 8)]), vec![(1, 8)]);
    }

    #[test]
    fn touching_ranges_merge() {
        assert_eq!(merge(vec![(1, 3), (4, 6)]), vec![(1, 6)]);
    }

    #[test]
    fn contained() {
        assert_eq!(merge(vec![(1, 10), (2, 3)]), vec![(1, 10)]);
    }

    #[test]
    fn unsorted_and_disjoint() {
        assert_eq!(merge(vec![(10, 12), (1, 2), (5, 6)]), vec![(1, 2), (5, 6), (10, 12)]);
    }

    #[test]
    fn empty() {
        assert_eq!(merge(vec![]), vec![]);
    }
}

use photocraft_legacy_corpus::{LegacyPath, generate, root};

use crate::corpus;

#[test]
fn ret_corpus_coverage_every_legacy_path_has_a_member() {
    assert_eq!(corpus().manifest().coverage(&LegacyPath::ALL.map(LegacyPath::name)).unwrap(), vec![]);
}

#[test]
fn the_committed_corpus_is_what_the_generator_makes() {
    assert_eq!(generate().unwrap().check(&root()).unwrap(), vec![]);
}

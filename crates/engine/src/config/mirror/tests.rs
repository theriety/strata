//! Unit tests for mirror-template parsing, overlap detection, and validation.

use super::*;

fn mirror_template(template: &str) -> MirrorTemplate {
    let parsed = MirrorTemplate::parse(template);
    assert!(
        parsed.is_some(),
        "test mirror template must parse: {template}"
    );
    parsed.unwrap_or_default()
}

#[test]
fn should_detect_overlap_when_stem_captures_have_unequal_lengths() {
    let prefixed = mirror_template("root/{dir}/pre-{stem}.ts");
    let suffixed = mirror_template("root/{dir}/{stem}-post.ts");

    assert!(mirror_templates_overlap(&prefixed, &suffixed));
    assert!(mirror_templates_overlap(&suffixed, &prefixed));
}

#[test]
fn should_capture_zero_single_and_multiple_directory_segments() {
    let template = mirror_template("root/{dir}/item-{stem}.ts");

    for (path, expected_dir) in [
        ("root/item-alpha.ts", ""),
        ("root/one/item-alpha.ts", "one"),
        ("root/one/two/item-alpha.ts", "one/two"),
    ] {
        let captures = template.captures(path);
        assert_eq!(
            captures.as_ref().map(|value| value.dir.as_str()),
            Some(expected_dir)
        );
        assert_eq!(
            captures.as_ref().map(|value| value.stem.as_str()),
            Some("alpha")
        );
    }
}

#[test]
fn should_intersect_directory_languages_at_zero_single_and_multiple_segments() {
    for right in [
        "root/{dir}/{stem}.ts",
        "root/one/{dir}/{stem}.ts",
        "root/one/two/{dir}/{stem}.ts",
    ] {
        let left = mirror_template("root/{dir}/{stem}.ts");
        let right = mirror_template(right);
        assert!(mirror_templates_overlap(&left, &right));
        assert!(mirror_templates_overlap(&right, &left));
    }
}

#[test]
fn should_distinguish_intersecting_and_disjoint_stem_affixes() {
    let broad = mirror_template("root/{dir}/pre-{stem}-post.ts");
    let intersecting = mirror_template("root/{dir}/pre-x{stem}-post.ts");
    let disjoint = mirror_template("root/{dir}/other-{stem}-post.ts");

    assert!(mirror_templates_overlap(&broad, &intersecting));
    assert!(!mirror_templates_overlap(&broad, &disjoint));
}

#[test]
fn should_compute_template_overlap_symmetrically_and_deterministically() {
    let templates = [
        mirror_template("root/{dir}/{stem}.ts"),
        mirror_template("root/fixed/{dir}/pre-{stem}.ts"),
        mirror_template("other/{dir}/{stem}-post.ts"),
    ];

    for left in &templates {
        for right in &templates {
            let expected = mirror_templates_overlap(left, right);
            assert_eq!(mirror_templates_overlap(right, left), expected);
            assert_eq!(mirror_templates_overlap(left, right), expected);
        }
    }
}

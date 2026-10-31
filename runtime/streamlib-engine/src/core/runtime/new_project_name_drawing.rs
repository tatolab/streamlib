// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The folder a project created with no directory is given: `<adjective>-<noun>`,
//! drawn at random and created under the streams root.

use std::path::{Path, PathBuf};

use crate::core::{Error, Result};

/// How many names are drawn before the draw is refused.
const NEW_PROJECT_NAME_DRAWS_BEFORE_REFUSING: usize = 8;

/// The folder under `$HOME` new projects are drawn into.
#[cfg(not(feature = "machine-directories-under-a-test-root"))]
const STREAMS_ROOT_FOLDER_INSIDE_HOME: &str = "streams";

const NEW_PROJECT_NAME_ADJECTIVES: &[&str] = &[
    "amber", "ample", "azure", "balmy", "bold", "brave", "breezy", "bright", "brisk", "bronze",
    "calm", "candid", "cheery", "chilly", "civic", "clever", "cloudy", "coastal", "cobalt", "cool",
    "copper", "cozy", "crimson", "crisp", "curly", "daring", "dapper", "deep", "dewy", "dusky",
    "eager", "early", "earnest", "easy", "electric", "elegant", "emerald", "even", "fabled",
    "fair", "fancy", "fast", "fearless", "fiery", "fleet", "floral", "fluent", "focal", "frank",
    "free", "fresh", "frosty", "gentle", "giant", "gilded", "glad", "gleaming", "glossy", "golden",
    "grand", "granite", "grassy", "great", "green", "hardy", "hazel", "hearty", "hidden", "hollow",
    "honest", "humble", "icy", "indigo", "inky", "ivory", "jade", "jolly", "jovial", "keen",
    "kind", "lanky", "large", "lasting", "lavish", "leafy", "level", "light", "lilac", "limber",
    "lithe", "lively", "lofty", "loyal", "lucid", "lucky", "lunar", "mellow", "merry", "mighty",
    "mild", "misty", "modest", "molten", "mossy", "narrow", "neat", "nimble", "noble", "northern",
    "oaken", "ocean", "olive", "opal", "open", "orange", "pale", "patient", "pearly", "perky",
    "pine", "plucky", "polar", "polished", "prime", "proud", "pure", "quick", "quiet", "radiant",
    "rapid", "rare", "ready", "regal", "rich", "rocky", "rosy", "royal", "ruby", "rugged",
    "rustic", "sandy", "scarlet", "serene", "sharp", "shiny", "silent", "silver", "simple",
    "sleek", "slender", "smooth", "snowy", "solar", "solid", "sonic", "sound", "spare", "speedy",
    "spry", "stable", "stark", "steady", "steep", "stellar", "stony", "stout", "strong", "sturdy",
    "sunny", "swift", "tall", "tame", "tawny", "tender", "thrifty", "tidal", "tidy", "timber",
    "tranquil", "true", "tulip", "upbeat", "urban", "valiant", "velvet", "vivid", "warm", "wary",
    "wavy", "wild", "windy", "wise", "witty", "woody", "young", "zany", "zesty",
];

const NEW_PROJECT_NAME_NOUNS: &[&str] = &[
    "acorn", "alder", "anchor", "antler", "apple", "arbor", "arch", "aspen", "atlas", "aurora",
    "badger", "bamboo", "banner", "barley", "basin", "beacon", "beaver", "birch", "bison", "bloom",
    "bluff", "boulder", "bramble", "breeze", "brook", "buffalo", "butte", "cactus", "canyon",
    "cardinal", "cascade", "cedar", "chestnut", "cinder", "citadel", "clover", "comet", "compass",
    "condor", "coral", "cove", "crane", "creek", "crest", "cricket", "crow", "cypress", "dahlia",
    "delta", "dune", "eagle", "echo", "egret", "elm", "ember", "falcon", "fern", "finch", "fjord",
    "flare", "flint", "forest", "fox", "gale", "garnet", "gazelle", "geyser", "glacier", "glade",
    "glen", "grove", "gull", "harbor", "hare", "hawk", "heron", "hill", "horizon", "iris",
    "island", "ivy", "jackal", "jaguar", "jasper", "juniper", "kestrel", "kettle", "koala",
    "lagoon", "lantern", "larch", "lark", "laurel", "lily", "lynx", "magnet", "maple", "marsh",
    "meadow", "mesa", "meteor", "mint", "moose", "moth", "nebula", "needle", "nettle", "oasis",
    "ocelot", "orbit", "orchid", "osprey", "otter", "owl", "panda", "panther", "parrot", "peak",
    "pebble", "pelican", "pepper", "petal", "pigeon", "pine", "planet", "plover", "pond", "poplar",
    "prairie", "puffin", "quail", "quarry", "quartz", "rabbit", "raven", "reef", "ridge", "river",
    "robin", "rocket", "sage", "salmon", "sapling", "sequoia", "shore", "sparrow", "spruce",
    "squirrel", "starling", "stone", "summit", "swallow", "swan", "thicket", "thistle", "thrush",
    "thunder", "tiger", "timber", "topaz", "trail", "tulip", "tundra", "valley", "vapor", "violet",
    "walnut", "warbler", "wave", "willow", "wolf", "wren", "yarrow", "zenith", "zephyr",
];

/// Draw a new project's folder under the streams root, created owner-
/// writable: `$HOME/streams`, or `<root>/streams` under a test build's machine
/// root, created when missing.
pub(crate) fn draw_new_project_directory_under_the_streams_root() -> Result<PathBuf> {
    let streams_root = the_streams_root_new_projects_are_drawn_under()?;
    std::fs::create_dir_all(&streams_root).map_err(|not_created| {
        Error::Configuration(format!(
            "the streams root {} could not be created, so no project was created: {not_created}",
            streams_root.display()
        ))
    })?;
    draw_new_project_directory_under(&streams_root)
}

/// Draw `<adjective>-<noun>` under `streams_root` and create it with
/// `fs::create_dir`, so a name already taken, or taken by a concurrent create,
/// draws again; refused naming the root after
/// [`NEW_PROJECT_NAME_DRAWS_BEFORE_REFUSING`] draws.
pub(crate) fn draw_new_project_directory_under(streams_root: &Path) -> Result<PathBuf> {
    let mut last_refusal = None;
    for _ in 0..NEW_PROJECT_NAME_DRAWS_BEFORE_REFUSING {
        let project_directory = streams_root.join(a_new_project_name_drawn_at_random());
        match std::fs::create_dir(&project_directory) {
            Ok(()) => return Ok(project_directory),
            Err(not_created) => last_refusal = Some(not_created),
        }
    }
    Err(Error::Configuration(format!(
        "no folder for a new project could be created under {} in \
         {NEW_PROJECT_NAME_DRAWS_BEFORE_REFUSING} draws; the last: {}; name the project's \
         directory instead",
        streams_root.display(),
        last_refusal
            .map(|refusal| refusal.to_string())
            .unwrap_or_default()
    )))
}

/// One `<adjective>-<noun>`, its randomness taken from a v4 UUID's bytes.
fn a_new_project_name_drawn_at_random() -> String {
    let random_bytes = uuid::Uuid::new_v4().into_bytes();
    let adjective_draw = u32::from_le_bytes([
        random_bytes[0],
        random_bytes[1],
        random_bytes[2],
        random_bytes[3],
    ]) as usize;
    let noun_draw = u32::from_le_bytes([
        random_bytes[8],
        random_bytes[9],
        random_bytes[10],
        random_bytes[11],
    ]) as usize;
    format!(
        "{}-{}",
        NEW_PROJECT_NAME_ADJECTIVES[adjective_draw % NEW_PROJECT_NAME_ADJECTIVES.len()],
        NEW_PROJECT_NAME_NOUNS[noun_draw % NEW_PROJECT_NAME_NOUNS.len()]
    )
}

#[cfg(not(feature = "machine-directories-under-a-test-root"))]
fn the_streams_root_new_projects_are_drawn_under() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::Configuration(
                "HOME is not set, so a new project has no streams root to be drawn under; name \
                 its directory instead"
                    .to_string(),
            )
        })?;
    if !home.is_absolute() {
        return Err(Error::Configuration(format!(
            "HOME is {}, which is not an absolute path, so a new project has no streams root to \
             be drawn under; name its directory instead",
            home.display()
        )));
    }
    Ok(home.join(STREAMS_ROOT_FOLDER_INSIDE_HOME))
}

#[cfg(feature = "machine-directories-under-a-test-root")]
fn the_streams_root_new_projects_are_drawn_under() -> Result<PathBuf> {
    streamlib_runtime_client_contract::machine_directories_test_root::TestMachineRoot::from_the_environment()
        .map(|test_machine_root| test_machine_root.path().join("streams"))
        .map_err(|refused| Error::Configuration(refused.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::cast_exposed_name_to_url_safe;

    #[test]
    fn every_word_is_a_fixed_point_of_the_exposed_name_cast_and_no_word_repeats() {
        for words in [NEW_PROJECT_NAME_ADJECTIVES, NEW_PROJECT_NAME_NOUNS] {
            for word in words {
                assert_eq!(cast_exposed_name_to_url_safe(word).unwrap(), *word);
            }
            let mut sorted = words.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), words.len());
        }
    }

    #[test]
    fn a_draw_creates_an_adjective_noun_folder_and_two_draws_never_share_one() {
        let streams_root = tempfile::tempdir().unwrap();
        let first = draw_new_project_directory_under(streams_root.path()).unwrap();
        let second = draw_new_project_directory_under(streams_root.path()).unwrap();
        assert!(first.is_dir() && second.is_dir());
        assert_ne!(first, second);
        let name = first.file_name().unwrap().to_str().unwrap();
        let (adjective, noun) = name.split_once('-').unwrap();
        assert!(NEW_PROJECT_NAME_ADJECTIVES.contains(&adjective), "{name}");
        assert!(NEW_PROJECT_NAME_NOUNS.contains(&noun), "{name}");
    }

    #[test]
    fn a_root_that_cannot_hold_a_folder_is_refused_naming_it_after_its_draws() {
        let streams_root = tempfile::tempdir().unwrap();
        let not_a_directory = streams_root.path().join("a-file");
        std::fs::write(&not_a_directory, b"").unwrap();
        let refusal = draw_new_project_directory_under(&not_a_directory).unwrap_err();
        assert!(
            refusal
                .to_string()
                .contains(&not_a_directory.display().to_string()),
            "{refusal}"
        );
    }
}

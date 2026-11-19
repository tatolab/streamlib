// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn groups(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(node_type, group)| ((*node_type).to_owned(), (*group).to_owned()))
        .collect()
}

fn filed(category: Option<&str>, from: Option<NodeTypeCategoryFrom>) -> FiledAs {
    (category.map(str::to_owned), from)
}

type FiledAs = (Option<String>, Option<NodeTypeCategoryFrom>);

/// Where `groups` files `node_type`, whose author declared `declared`.
fn filed_by(groups: &NodeTypeGroupsOfAProject, node_type: &str, declared: Option<&str>) -> FiledAs {
    let mut category = declared.map(str::to_owned);
    let mut category_from = None;
    groups.file(node_type, &mut category, &mut category_from);
    (category, category_from)
}

#[test]
fn the_projects_own_map_wins_over_the_imports_and_the_imports_over_the_declaration() {
    let of_the_cell = NodeTypeGroupsOfAProject {
        own: groups(&[
            ("streamlib::core::Posterize", "Effects/Stylize"),
            ("imports/robot-arm/arm_nodes:Wrist", "Cell/Arm"),
        ]),
        of_each_import: vec![
            NodeTypeGroupsOfAnImport {
                folder: "robot-arm".to_owned(),
                groups: groups(&[
                    ("arm_nodes:Gripper", "Robot/Hands"),
                    ("arm_nodes:Wrist", "Robot/Hands"),
                ]),
            },
            NodeTypeGroupsOfAnImport {
                folder: "lights".to_owned(),
                groups: groups(&[("lights_nodes:Dimmer", "Lights")]),
            },
        ],
    };

    assert_eq!(
        filed_by(&of_the_cell, "streamlib::core::Posterize", Some("Color")),
        filed(Some("Effects/Stylize"), Some(NodeTypeCategoryFrom::Project))
    );
    assert_eq!(
        filed_by(&of_the_cell, "imports/robot-arm/arm_nodes:Wrist", None),
        filed(Some("Cell/Arm"), Some(NodeTypeCategoryFrom::Project))
    );
    assert_eq!(
        filed_by(&of_the_cell, "imports/robot-arm/arm_nodes:Gripper", None),
        filed(Some("Robot/Hands"), Some(NodeTypeCategoryFrom::Import)),
        "an import names its own node types without its folder"
    );
    assert_eq!(
        filed_by(
            &of_the_cell,
            "imports/lights/lights_nodes:Dimmer",
            Some("Dim")
        ),
        filed(Some("Lights"), Some(NodeTypeCategoryFrom::Import))
    );
    assert_eq!(
        filed_by(&of_the_cell, "streamlib::core::Invert", Some("Color")),
        filed(Some("Color"), Some(NodeTypeCategoryFrom::Declaration))
    );
    assert_eq!(
        filed_by(&of_the_cell, "streamlib::core::Invert", None),
        filed(None, None)
    );
}

#[test]
fn an_imports_map_files_only_the_node_types_the_import_holds() {
    let of_the_cell = NodeTypeGroupsOfAProject {
        own: BTreeMap::new(),
        of_each_import: vec![NodeTypeGroupsOfAnImport {
            folder: "robot-arm".to_owned(),
            groups: groups(&[
                ("streamlib::core::Blur", "Arm/Looks"),
                ("arm_nodes:Gripper", "Robot/Hands"),
            ]),
        }],
    };

    assert_eq!(
        filed_by(&of_the_cell, "arm_nodes:Gripper", Some("Grip")),
        filed(Some("Grip"), Some(NodeTypeCategoryFrom::Declaration)),
        "the cell's own node type of the same name is the cell's"
    );
    assert_eq!(
        filed_by(&of_the_cell, "streamlib::core::Blur", None),
        filed(None, None),
        "a built-in is not the import's to file"
    );
    assert_eq!(
        filed_by(&of_the_cell, "imports/lights/arm_nodes:Gripper", None),
        filed(None, None),
        "another import's node type of the same name is not this import's"
    );
}

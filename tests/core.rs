use async_channel::unbounded;
use codex_image::layout::{Position, compute_layout};
use codex_image::manifest;
use codex_image::model::{BoardNode, NewNodesRequest, NodeStatus};
use codex_image::storage::{DataPaths, Repository};
use std::collections::{BTreeMap, HashMap};
use tempfile::TempDir;

fn node(id: &str, parent: Option<&str>, created_at: i64) -> BoardNode {
    BoardNode {
        id: id.into(),
        parent_id: parent.map(str::to_owned),
        prompt: id.into(),
        aspect: "auto".into(),
        source_images: vec![],
        attachments: vec![],
        images: vec![],
        image_labels: vec![],
        attempts: vec![],
        text: String::new(),
        status: NodeStatus::Done,
        error: None,
        stop_reason: None,
        x: None,
        y: None,
        created_at,
        run_started_at: None,
        finished_at: None,
        usage: Some(BTreeMap::new()),
    }
}

#[test]
fn manifest_rejects_duplicates_and_relative_paths() {
    let duplicate = r#"{"summary":"done","complete":true,"outputs":[{"path":"/a.png","label":"A"},{"path":"/a.png","label":"B"}]}"#;
    assert!(manifest::parse(duplicate).is_err());
    assert!(manifest::absolute_file_path("relative.png").is_err());
}

#[test]
fn layout_places_children_below_parent_and_preserves_manual_nodes() {
    let mut nodes = vec![
        node("root", None, 1),
        node("a", Some("root"), 2),
        node("b", Some("root"), 3),
    ];
    nodes[1].x = Some(900.0);
    nodes[1].y = Some(250.0);
    let layout = compute_layout(&nodes, &HashMap::new());
    assert_eq!(layout["a"], Position { x: 900.0, y: 250.0 });
    assert!(layout["b"].y > layout["root"].y);
}

#[test]
fn repository_persists_and_restores_deleted_subtrees() {
    let directory = TempDir::new().unwrap();
    let generated = directory.path().join("generated");
    std::fs::create_dir_all(&generated).unwrap();
    let (sender, _receiver) = unbounded();
    let repository = Repository::open_at(
        DataPaths::at(directory.path().join("data"), generated),
        sender,
    )
    .unwrap();
    let board = repository.create_board().unwrap();
    let root = repository
        .add_nodes(
            &board.id,
            NewNodesRequest {
                prompt: "root".into(),
                parent_id: None,
                source_images: None,
                aspect: "auto".into(),
                count: 1,
                attachment_paths: vec![],
                attachment_urls: vec![],
            },
        )
        .unwrap()
        .remove(0);
    let child = repository
        .add_nodes(
            &board.id,
            NewNodesRequest {
                prompt: "child".into(),
                parent_id: Some(root.id.clone()),
                source_images: None,
                aspect: "1:1".into(),
                count: 1,
                attachment_paths: vec![],
                attachment_urls: vec![],
            },
        )
        .unwrap()
        .remove(0);
    let (deleted, undo_id) = repository.delete_subtree(&board.id, &root.id).unwrap();
    assert_eq!(deleted.len(), 2);
    assert!(repository.board(&board.id).unwrap().nodes.is_empty());
    repository.undo_delete(&board.id, &undo_id).unwrap();
    let restored = repository.board(&board.id).unwrap();
    assert_eq!(restored.nodes.len(), 2);
    assert!(restored.nodes.iter().any(|node| node.id == child.id));
}

#[test]
fn repository_migrates_legacy_images_and_recovers_interrupted_runs() {
    let directory = TempDir::new().unwrap();
    let data = directory.path().join("data");
    let generated = directory.path().join("generated");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&generated).unwrap();
    let legacy = serde_json::json!([{
        "id": "board",
        "title": "Legacy",
        "createdAt": 1,
        "nodes": [{
            "id": "node",
            "parentId": null,
            "prompt": "legacy",
            "aspect": "1:1",
            "sourceImages": [],
            "attachments": [],
            "images": ["/images/board/old.png"],
            "imageLabels": [],
            "text": "",
            "status": "running",
            "error": null,
            "stopReason": null,
            "x": null,
            "y": null,
            "createdAt": 2,
            "runStartedAt": 2,
            "finishedAt": null,
            "usage": null
        }]
    }]);
    std::fs::write(
        data.join("boards.json"),
        serde_json::to_vec_pretty(&legacy).unwrap(),
    )
    .unwrap();
    let (sender, _receiver) = unbounded();
    let repository = Repository::open_at(DataPaths::at(data, generated), sender).unwrap();
    let node = repository.node("board", "node").unwrap();
    assert_eq!(node.attempts, node.images);
    assert_eq!(node.image_labels, ["Output 1".to_owned()]);
    assert_eq!(node.status, NodeStatus::Error);
    assert!(node.error.unwrap().contains("closed unexpectedly"));
}

//! The instructions handed to Codex: the standing image-generation contract,
//! the per-node request, and the recovery prompt used to salvage a run.

use crate::model::{Board, BoardNode};
use crate::storage::Repository;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const PREAMBLE: &str = r#"You are an expert image-generation assistant.

Hard rules:
- ALWAYS create every final visual deliverable implied by the request with your built-in image generation tool. Never draw images with code (SVG/HTML/canvas), never substitute placeholders, and never fetch images from the web.
- Infer the number of final deliverables from the request. A single scene normally needs one; a ten-page comic needs ten separate ordered images. Never combine multiple requested deliverables into a contact sheet or collage unless the user explicitly asks for that format.
- You may call the image generation tool again whenever an output needs correction. At the end, select the best final result for each intended deliverable and omit every superseded attempt.
- The app captures generated files automatically. Do NOT run shell commands to copy, move, inspect, or verify image files unless the user explicitly asks for file operations.
- Your final response must follow the supplied JSON schema. Put only selected final images in `outputs`, in the semantic order requested by the user. For each output, use the exact absolute saved path returned by the image generation tool and a short identifying label. Never include a superseded attempt. Set `complete` to true only when the selected outputs fulfill the entire request; otherwise set it to false. Keep `summary` to one concise sentence.
- Structured progress updates are not final selections: while any render is pending, set `complete` to false and leave `outputs` empty. Populate `outputs` only in the terminal response after every render and correction has settled.

Prompting the image tool:
- Rewrite the request into a clean spec ordered scene/backdrop -> subject -> key details -> constraints, and include the intended use to set the polish level. For complex requests use short labeled lines.
- Match augmentation to specificity. Never invent characters, props, brands, slogans, palettes, or story beats the user did not imply.
- For photorealism, use photography language and ask for real-world texture and imperfect everyday detail.
- If text must appear in the image, quote it verbatim, specify typography and placement, spell uncommon words letter-by-letter, and require exact rendering with no extra characters.
- When image files are provided, treat each by its stated role. For compositing, match lighting, perspective, and scale.
- For edits, state invariants explicitly. Preserve identity aggressively when people are involved and preserve everything the request does not ask to change."#;

pub fn build_node_prompt(
    repository: &Repository,
    board: &Board,
    node: &BoardNode,
    source_paths: &[PathBuf],
    same_run_conditioner: Option<(&Path, &Path)>,
    index: usize,
    count: usize,
) -> String {
    let mut sections = vec![PREAMBLE.to_owned()];
    let by_id: HashMap<_, _> = board
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let ancestors = prompt_chain(&by_id, node.parent_id.as_deref());
    if !ancestors.is_empty() {
        sections.push(format!(
            "This request continues earlier work on an image. The prompts so far, oldest first:\n{}",
            numbered(&ancestors)
        ));
    }
    let merged: Vec<_> = node
        .merged_from
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).copied())
        .collect();
    for (index, other) in merged.iter().enumerate() {
        let chain = prompt_chain(&by_id, Some(&other.id));
        sections.push(format!(
            "Image {} below comes from a separate chain of work on the same board. Its prompts so far, oldest first:\n{}",
            index + 2,
            numbered(&chain)
        ));
    }
    if !source_paths.is_empty() && merged.is_empty() {
        sections.push(format!(
            "The current image to continue from is saved at:\n{}\nView it first. The request below applies to this image: keep everything it does not ask to change.",
            bullet_paths(source_paths)
        ));
    } else if !source_paths.is_empty() {
        sections.push(format!(
            "The images to combine are saved at, image 1 first:\n{}\nView them all first. The request below applies to these images together: carry over the identities, styles, and details it names from each one.",
            bullet_paths(source_paths)
        ));
    }
    let attachments: Vec<_> = node
        .attachments
        .iter()
        .filter_map(|url| repository.image_path(&board.id, url))
        .filter(|path| path.exists())
        .collect();
    if !attachments.is_empty() {
        sections.push(format!(
            "The user attached reference image file(s). View them before generating:\n{}",
            bullet_paths(&attachments)
        ));
    }
    if let Some((executable, directory)) = same_run_conditioner {
        sections.push(same_run_conditioning_section(executable, directory));
    }
    sections.push(format!("Request: {}", node.prompt));
    let mut extras = Vec::new();
    if node.aspect != "auto" {
        extras.push(format!("Aspect ratio: {}.", node.aspect));
    }
    if count > 1 {
        extras.push(format!(
            "{count} independent takes of this entire request are generated in parallel; this is take {}. Give this take its own distinct interpretation while still producing every final deliverable implied by the request.",
            index + 1
        ));
    }
    if !extras.is_empty() {
        sections.push(extras.join(" "));
    }
    sections.join("\n\n")
}

fn same_run_conditioning_section(executable: &Path, directory: &Path) -> String {
    let command = conditioning_command(executable, &directory.join("step-N.png"));
    format!(
        r#"Same-run generated-image dependencies:
- When an image generated in this run will be supplied to any later image-generation call, condition it synchronously first. This is the sole permitted exception to the shell-file-operation prohibition above.
- For every generated input, replace `<RAW_GENERATED_PATH>` and `N`, then run exactly: `{command}`
- Pass only the resulting PNG path(s) to the later image-generation call through `referenced_image_paths`. Do not use `num_last_images_to_include` or conversation-carried raw images for a dependent call.
- Do not condition independent outputs that will not be reused as image inputs.
- Conditioned PNGs are intermediate inputs only. Never put them in the final `outputs`; final selections must still use the raw absolute paths returned by the image-generation tool."#
    )
}

#[cfg(not(target_os = "windows"))]
fn conditioning_command(executable: &Path, destination: &Path) -> String {
    format!(
        "{} --condition-image '<RAW_GENERATED_PATH>' {}",
        shell_quote(executable),
        shell_quote(destination),
    )
}

#[cfg(target_os = "windows")]
fn conditioning_command(executable: &Path, destination: &Path) -> String {
    // Codex uses PowerShell on native Windows. The call operator is required
    // when a quoted executable path contains spaces.
    format!(
        "& {} --condition-image '<RAW_GENERATED_PATH>' {}",
        powershell_quote(executable),
        powershell_quote(destination),
    )
}

#[cfg(not(target_os = "windows"))]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(target_os = "windows")]
fn powershell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

pub fn selection_recovery_prompt<'a>(
    node: &BoardNode,
    candidates: impl Iterator<Item = &'a PathBuf>,
    failure: Option<&str>,
) -> String {
    let mut lines = vec![
        "You are finalizing an interrupted image-generation run.".to_owned(),
        "Hard rules: Do not generate, edit, copy, move, or delete images. View only the candidates below. Select the strongest final candidate for each intended deliverable in semantic order. Omit superseded attempts and duplicates. Return the supplied JSON schema and set complete accurately.".to_owned(),
        format!("Original request: {}", node.prompt),
    ];
    if let Some(failure) = failure {
        lines.push(format!("Generation interruption: {failure}"));
    }
    lines.push("Candidate files:".into());
    lines.extend(
        candidates
            .enumerate()
            .map(|(index, path)| format!("{}. {}", index + 1, path.display())),
    );
    lines.join("\n\n")
}

/// The prompts of `start` and its ancestors, oldest first, trimmed to what a
/// history section can usefully carry.
fn prompt_chain(by_id: &HashMap<&str, &BoardNode>, start: Option<&str>) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = start.and_then(|id| by_id.get(id).copied());
    while let Some(ancestor) = current {
        let prompt = if ancestor.prompt.chars().count() > 400 {
            format!("{}…", ancestor.prompt.chars().take(397).collect::<String>())
        } else {
            ancestor.prompt.clone()
        };
        chain.push(prompt);
        if chain.len() == 12 {
            break;
        }
        current = ancestor
            .parent_id
            .as_deref()
            .and_then(|id| by_id.get(id).copied());
    }
    chain.reverse();
    chain
}

fn numbered(prompts: &[String]) -> String {
    prompts
        .iter()
        .enumerate()
        .map(|(index, prompt)| format!("{}. {prompt}", index + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

fn bullet_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn tail_chars(value: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    match value.char_indices().rev().nth(limit) {
        Some((index, character)) => value[index + character.len_utf8()..].to_owned(),
        None => value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{build_node_prompt, same_run_conditioning_section, tail_chars};
    use crate::model::{Board, BoardNode, NodeStatus};
    use crate::storage::{DataPaths, Repository};
    use std::path::{Path, PathBuf};

    fn node(id: &str, parent_id: Option<&str>, merged_from: &[&str]) -> BoardNode {
        BoardNode {
            id: id.into(),
            parent_id: parent_id.map(str::to_owned),
            merged_from: merged_from.iter().map(|id| (*id).to_owned()).collect(),
            prompt: format!("prompt {id}"),
            aspect: "auto".into(),
            source_images: Vec::new(),
            attachments: Vec::new(),
            images: Vec::new(),
            image_labels: Vec::new(),
            attempts: Vec::new(),
            text: String::new(),
            status: NodeStatus::Done,
            error: None,
            stop_reason: None,
            x: None,
            y: None,
            created_at: 0,
            run_started_at: None,
            finished_at: None,
            usage: None,
        }
    }

    /// A combined request tells Codex where every image comes from: the
    /// parent chain as before, plus each merged chain numbered to match the
    /// order of the image paths.
    #[test]
    fn combined_images_carry_every_chain_history() {
        let directory = tempfile::TempDir::new().unwrap();
        let (sender, _receiver) = async_channel::unbounded();
        let repository = Repository::open_at(
            DataPaths::at(
                directory.path().join("data"),
                directory.path().join("generated"),
            ),
            sender,
        )
        .unwrap();
        let board = Board {
            id: "board".into(),
            title: "Board".into(),
            created_at: 0,
            image_sizes: Default::default(),
            nodes: vec![
                node("hero", None, &[]),
                node("hero-armor", Some("hero"), &[]),
                node("villain", None, &[]),
                node("crossover", Some("hero-armor"), &["villain", "gone"]),
            ],
        };
        let sources = [
            PathBuf::from("/img/hero-armor.png"),
            PathBuf::from("/img/villain.png"),
        ];

        let prompt = build_node_prompt(&repository, &board, &board.nodes[3], &sources, None, 0, 1);

        assert!(
            prompt.contains(
                "The prompts so far, oldest first:\n1. prompt hero\n2. prompt hero-armor"
            )
        );
        assert!(prompt.contains("Image 2 below comes from a separate chain of work on the same board. Its prompts so far, oldest first:\n1. prompt villain"));
        assert!(!prompt.contains("Image 3 below"));
        assert!(prompt.contains("The images to combine are saved at, image 1 first:\n- /img/hero-armor.png\n- /img/villain.png"));

        let plain = build_node_prompt(
            &repository,
            &board,
            &board.nodes[1],
            &sources[..1],
            None,
            0,
            1,
        );
        assert!(plain.contains("The current image to continue from is saved at:"));
        assert!(!plain.contains("separate chain"));
    }

    #[test]
    fn same_run_instructions_force_conditioned_file_handoffs() {
        #[cfg(not(target_os = "windows"))]
        let instructions = same_run_conditioning_section(
            Path::new("/Applications/CodexImage.app/codex-image"),
            Path::new("/tmp/work space/same-run"),
        );
        #[cfg(target_os = "windows")]
        let instructions = same_run_conditioning_section(
            Path::new(r"C:\Program Files\CodexImage\CodexImage.exe"),
            Path::new(r"C:\Temp\work space\same-run"),
        );

        assert!(instructions.contains("--condition-image"));
        assert!(instructions.contains("referenced_image_paths"));
        assert!(instructions.contains("Do not use `num_last_images_to_include`"));
        assert!(instructions.contains("Never put them in the final `outputs`"));
        #[cfg(not(target_os = "windows"))]
        {
            assert!(instructions.contains("'/Applications/CodexImage.app/codex-image'"));
            assert!(instructions.contains("'/tmp/work space/same-run/step-N.png'"));
        }
        #[cfg(target_os = "windows")]
        {
            assert!(instructions.contains("& 'C:\\Program Files\\CodexImage\\CodexImage.exe'"));
            assert!(instructions.contains("'C:\\Temp\\work space\\same-run\\step-N.png'"));
        }
    }

    #[test]
    fn activity_tails_keep_the_requested_unicode_characters() {
        assert_eq!(tail_chars("a🙂бcd", 3), "бcd");
        assert_eq!(tail_chars("a🙂бcd", 20), "a🙂бcd");
        assert_eq!(tail_chars("a🙂бcd", 0), "");
    }
}

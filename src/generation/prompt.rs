//! The instructions handed to Codex: the standing image-generation contract,
//! the per-node request, and the recovery prompt used to salvage a run.

use crate::model::{Board, BoardNode, CHAT_CONTEXT_TURNS, ChatRole, NodeStatus};
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

/// The knobs of one generation run that the board itself does not carry.
pub struct RunOptions<'a> {
    /// Conditioning executable and directory, when same-run conditioning is on.
    pub conditioner: Option<(&'a Path, &'a Path)>,
    /// How many earlier images of the card's chain travel as reference.
    pub lineage_refs: usize,
    /// This take's index among `takes` parallel takes of the same request.
    pub take: usize,
    pub takes: usize,
}

pub fn build_node_prompt(
    repository: &Repository,
    board: &Board,
    node: &BoardNode,
    source_paths: &[PathBuf],
    options: RunOptions<'_>,
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
    if !source_paths.is_empty()
        && let Some(section) =
            lineage_reference_section(repository, board, &by_id, node, options.lineage_refs)
    {
        sections.push(section);
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
    if let Some((executable, directory)) = options.conditioner {
        sections.push(same_run_conditioning_section(executable, directory));
    }
    sections.push(format!("Request: {}", node.prompt));
    let mut extras = Vec::new();
    if node.aspect != "auto" {
        extras.push(format!("Aspect ratio: {}.", node.aspect));
    }
    if options.takes > 1 {
        extras.push(format!(
            "{} independent takes of this entire request are generated in parallel; this is take {}. Give this take its own distinct interpretation while still producing every final deliverable implied by the request.",
            options.takes,
            options.take + 1
        ));
    }
    if !extras.is_empty() {
        sections.push(extras.join(" "));
    }
    sections.join("\n\n")
}

/// Images from earlier steps of the same chain. A branch taken from a close-up
/// leaves Codex guessing at everything the crop hides, so the steps that did
/// show it come along as identity reference.
fn lineage_reference_section(
    repository: &Repository,
    board: &Board,
    by_id: &HashMap<&str, &BoardNode>,
    node: &BoardNode,
    limit: usize,
) -> Option<String> {
    if limit == 0 {
        return None;
    }
    let mut lines = Vec::new();
    let mut current = node
        .parent_id
        .as_deref()
        .and_then(|id| by_id.get(id).copied());
    while let Some(ancestor) = current {
        if lines.len() == limit {
            break;
        }
        if let Some(url) = ancestor.images.first()
            && !node.source_images.contains(url)
            && let Some(path) = repository
                .image_path(&board.id, url)
                .filter(|path| path.exists())
        {
            lines.push(format!(
                "- {} — produced by the earlier step: {}",
                path.display(),
                excerpt(&ancestor.prompt, 160)
            ));
        }
        current = ancestor
            .parent_id
            .as_deref()
            .and_then(|id| by_id.get(id).copied());
    }
    (!lines.is_empty()).then(|| {
        format!(
            "Earlier images from this same chain, nearest step first. They show the same subject and world before the current image was cropped, reframed, or changed:\n{}\nView them too. Use them only to keep identity, anatomy, proportions, wardrobe, and style consistent when the image being continued does not show those details. Do not copy their framing and do not treat them as images to edit.",
            lines.join("\n")
        )
    })
}

const CHAT_PREAMBLE: &str = r#"You are discussing one card on the user's image-generation board. This is a conversation, not a generation run.

Hard rules:
- Never call the image generation tool. Do not create, edit, copy, move, or delete any file.
- You may view the image files listed below, and you should before saying anything about what they show.
- Reply in plain text: no JSON, no markdown headings, no preamble such as "Certainly".
- Be concrete and brief. For ideas, give a short numbered list of distinct one-line options. For a result that came out wrong, name the likely cause and the prompt change that fixes it. When the user asks for a prompt, write the prompt itself, ready to paste."#;

/// The conversation about one card, with everything the card is made of so
/// Codex can answer about this specific result rather than in the abstract.
pub fn build_chat_prompt(repository: &Repository, board: &Board, node: &BoardNode) -> String {
    let mut sections = vec![CHAT_PREAMBLE.to_owned()];
    let by_id: HashMap<_, _> = board
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    sections.push(format!("Board: {}", board.title));
    let ancestors = prompt_chain(&by_id, node.parent_id.as_deref());
    if !ancestors.is_empty() {
        sections.push(format!(
            "This card continues earlier work. The prompts leading to it, oldest first:\n{}",
            numbered(&ancestors)
        ));
    }
    sections.push(format!("This card's request: {}", node.prompt));
    let mut facts = vec![format!(
        "Status: {}",
        match node.status {
            NodeStatus::Running => "still generating",
            NodeStatus::Done => "finished",
            NodeStatus::Error => "failed",
            NodeStatus::Stopped => "stopped by the user",
        }
    )];
    if node.aspect != "auto" {
        facts.push(format!("Aspect ratio: {}", node.aspect));
    }
    if !node.text.is_empty() {
        facts.push(format!("Codex's summary of the run: {}", node.text));
    }
    if let Some(error) = node.error.as_deref().filter(|error| !error.is_empty()) {
        facts.push(format!("Recorded failure: {error}"));
    }
    sections.push(facts.join("\n"));
    let paths = |urls: &[String]| -> Vec<PathBuf> {
        urls.iter()
            .filter_map(|url| repository.image_path(&board.id, url))
            .filter(|path| path.exists())
            .collect()
    };
    for (label, urls) in [
        ("The images this card produced", &node.images),
        ("The images it was generated from", &node.source_images),
        ("Reference images the user attached", &node.attachments),
    ] {
        let files = paths(urls);
        if !files.is_empty() {
            sections.push(format!("{label}:\n{}", bullet_paths(&files)));
        }
    }
    let transcript: Vec<_> = node
        .chat
        .iter()
        .rev()
        .take(CHAT_CONTEXT_TURNS)
        .rev()
        .map(|message| {
            let speaker = match message.role {
                ChatRole::User => "User",
                ChatRole::Agent => "You",
                ChatRole::Error => "System",
            };
            format!("{speaker}: {}", message.text)
        })
        .collect();
    sections.push(format!("Conversation so far:\n{}", transcript.join("\n\n")));
    sections.push("Reply to the last user message.".to_owned());
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
        chain.push(excerpt(&ancestor.prompt, 400));
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

fn excerpt(prompt: &str, limit: usize) -> String {
    if prompt.chars().count() > limit {
        format!(
            "{}…",
            prompt
                .chars()
                .take(limit.saturating_sub(3))
                .collect::<String>()
        )
    } else {
        prompt.to_owned()
    }
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
    use super::{
        RunOptions, build_chat_prompt, build_node_prompt, same_run_conditioning_section, tail_chars,
    };

    fn options(lineage_refs: usize) -> RunOptions<'static> {
        RunOptions {
            conditioner: None,
            lineage_refs,
            take: 0,
            takes: 1,
        }
    }
    use crate::model::{Board, BoardNode, ChatMessage, ChatRole, NodeStatus};
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
            chat: Vec::new(),
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

        let prompt = build_node_prompt(&repository, &board, &board.nodes[3], &sources, options(0));

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
            options(0),
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

    fn board_with(nodes: Vec<BoardNode>) -> Board {
        Board {
            id: "board".into(),
            title: "Board".into(),
            created_at: 0,
            image_sizes: Default::default(),
            nodes,
        }
    }

    /// A branch taken from a close-up carries the earlier full shots of its own
    /// chain as reference, and never re-lists the image it is already editing.
    #[test]
    fn chain_references_carry_earlier_images_but_not_the_source() {
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
        let images = repository.paths().images.join("board");
        std::fs::create_dir_all(&images).unwrap();
        for name in ["full.png", "face.png"] {
            std::fs::write(images.join(name), b"x").unwrap();
        }
        let mut full = node("full", None, &[]);
        full.images = vec!["/images/board/full.png".to_owned()];
        let mut face = node("face", Some("full"), &[]);
        face.images = vec!["/images/board/face.png".to_owned()];
        let mut child = node("child", Some("face"), &[]);
        child.source_images = face.images.clone();
        let board = board_with(vec![full, face, child]);
        let sources = [images.join("face.png")];

        let prompt = build_node_prompt(&repository, &board, &board.nodes[2], &sources, options(3));

        assert!(prompt.contains("Earlier images from this same chain"));
        assert!(prompt.contains("full.png — produced by the earlier step: prompt full"));
        // The image being continued is named once, by the section that owns it.
        assert_eq!(prompt.matches("face.png").count(), 1);

        let off = build_node_prompt(&repository, &board, &board.nodes[2], &sources, options(0));
        assert!(!off.contains("Earlier images from this same chain"));
    }

    #[test]
    fn chat_prompt_carries_the_transcript_and_bans_generating() {
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
        let mut card = node("card", None, &[]);
        card.chat = vec![
            ChatMessage {
                role: ChatRole::User,
                text: "why is the body wrong".into(),
                at: 0,
            },
            ChatMessage {
                role: ChatRole::Agent,
                text: "the crop hides it".into(),
                at: 1,
            },
        ];
        let board = board_with(vec![card]);

        let prompt = build_chat_prompt(&repository, &board, &board.nodes[0]);

        assert!(prompt.contains("Never call the image generation tool"));
        assert!(prompt.contains("User: why is the body wrong"));
        assert!(prompt.contains("You: the crop hides it"));
        assert!(prompt.ends_with("Reply to the last user message."));
    }

    #[test]
    fn activity_tails_keep_the_requested_unicode_characters() {
        assert_eq!(tail_chars("a🙂бcd", 3), "бcd");
        assert_eq!(tail_chars("a🙂бcd", 20), "a🙂бcd");
        assert_eq!(tail_chars("a🙂бcd", 0), "");
    }
}

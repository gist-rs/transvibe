use color_eyre::Result;
use crossterm::event::KeyModifiers;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use futures_util::StreamExt;
use kalosm::language::*;
use kalosm::sound::*;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

#[derive(Debug)]
enum AppUpdate {
    LiveJapaneseUpdate(String),
    JapaneseSegmentComplete(String),
    EnglishTranslation(String),
    SamplesProcessed(usize),
    RawSamplesDetected(usize),
    StatusUpdate(String),
    Error(String),
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum AppInputMode {
    Listening,
    StoppedTyping,
}

struct App {
    status: String,
    current_live_japanese: String,
    completed_japanese: Vec<String>,
    completed_translations: Vec<String>,
    rx: mpsc::Receiver<AppUpdate>,
    should_quit: bool,
    input_mode: AppInputMode,
    user_input: String, // For when typing is enabled
    // Shared state to control the audio processing task
    is_listening_shared: Arc<AtomicBool>,
    japanese_scroll_state: ScrollbarState,
    japanese_scroll: usize,
    english_scroll_state: ScrollbarState,
    english_scroll: u16,
    total_samples_listened: usize,
    raw_samples_count: usize,
}

impl App {
    fn new(rx: mpsc::Receiver<AppUpdate>) -> Self {
        Self {
            status: "Initializing... Press 's' to Stop/Start, 'q' to Quit".to_string(),
            current_live_japanese: String::new(),
            completed_japanese: Vec::new(),
            completed_translations: Vec::new(),
            rx,
            should_quit: false,
            input_mode: AppInputMode::Listening,
            user_input: String::new(),
            is_listening_shared: Arc::new(AtomicBool::new(true)), // Start in listening mode
            japanese_scroll_state: ScrollbarState::default(),
            japanese_scroll: 0,
            english_scroll_state: ScrollbarState::default(),
            english_scroll: 0,
            total_samples_listened: 0,
            raw_samples_count: 0,
        }
    }

    fn on_update(&mut self, update: AppUpdate) {
        match update {
            AppUpdate::StatusUpdate(s) => self.status = s,
            AppUpdate::LiveJapaneseUpdate(s) => self.current_live_japanese = s,
            AppUpdate::JapaneseSegmentComplete(jp_text) => {
                self.completed_japanese.insert(0, jp_text);
                self.current_live_japanese.clear();
                // Always insert a placeholder for the new Japanese text at the beginning
                self.completed_translations
                    .insert(0, "Translating...".to_string());

                // Defensive: Ensure translation list doesn't grow longer than Japanese list.
                while self.completed_translations.len() > self.completed_japanese.len() {
                    self.completed_translations.pop(); // Remove from the end (oldest assumed extras)
                }
            }
            AppUpdate::EnglishTranslation(en_text) => {
                let jp_len = self.completed_japanese.len();
                let tr_len = self.completed_translations.len();

                // Try to update the placeholder at the beginning (index 0), as it's the newest.
                if tr_len > 0 && self.completed_translations[0] == "Translating..." {
                    self.completed_translations[0] = en_text;
                }
                // Fallback: find the earliest "Translating..." placeholder and update it.
                // This covers cases where translations might arrive out of order for older segments.
                else if let Some(index) = self
                    .completed_translations
                    .iter()
                    .position(|t| t == "Translating...")
                {
                    self.completed_translations[index] = en_text;
                }
                // Further fallback: if no placeholder is found and lengths allow, insert new translation at the top.
                // This case should be rare if JapaneseSegmentComplete always adds a placeholder.
                else if tr_len < jp_len {
                    self.completed_translations.insert(0, en_text);
                }
                // If none of the above (e.g. tr_len >= jp_len and no placeholder found),
                // the translation might be an anomaly or for an already translated segment.
                // We'll let the cleanup logic below adjust list lengths if necessary.

                // Defensive: Ensure translation list doesn't grow excessively longer than Japanese list.
                while self.completed_translations.len() > self.completed_japanese.len() {
                    self.completed_translations.pop(); // Remove from the end
                }
                // Defensive: Ensure every Japanese text has a corresponding translation/placeholder.
                // New placeholders are inserted at the beginning to match the Japanese text insertion.
                while self.completed_translations.len() < self.completed_japanese.len() {
                    self.completed_translations
                        .insert(0, "[Pending Translation]".to_string());
                }
            }
            AppUpdate::SamplesProcessed(samples) => {
                self.total_samples_listened += samples;
                self.raw_samples_count = 0; // Reset after final samples for transcribed segment reported
            }
            AppUpdate::RawSamplesDetected(samples) => {
                self.raw_samples_count += samples;
            }
            AppUpdate::Error(err_msg) => {
                self.status = format!("ERROR: {}", err_msg);
                // Potentially log to a file or display more prominently
            }
        }
    }

    fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;
            self.handle_events()?;
            self.handle_updates();
        }
        Ok(())
    }

    fn handle_events(&mut self) -> Result<()> {
        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    // Global keybindings for scrolling, etc.
                    // Check for scroll events first, as they are global.
                    let mut event_handled = true; // Assume handled if it matches
                    match (key.code, key.modifiers) {
                        (KeyCode::Down, KeyModifiers::CONTROL)
                        | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                            self.scroll_english_down();
                        }
                        (KeyCode::Up, KeyModifiers::CONTROL)
                        | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                            self.scroll_english_up();
                        }
                        (KeyCode::Down, KeyModifiers::ALT)
                        | (KeyCode::Char('j'), KeyModifiers::ALT) => {
                            self.scroll_japanese_down();
                        }
                        (KeyCode::Up, KeyModifiers::ALT)
                        | (KeyCode::Char('k'), KeyModifiers::ALT) => {
                            self.scroll_japanese_up();
                        }
                        _ => {
                            event_handled = false; // Not a global scroll key
                        }
                    }

                    if event_handled {
                        return Ok(());
                    }

                    // Mode-specific keybindings
                    if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc {
                        self.should_quit = true;
                        return Ok(());
                    }
                    match self.input_mode {
                        AppInputMode::Listening => match key.code {
                            KeyCode::Char('q') => {
                                self.should_quit = true;
                                self.status = "Exiting...".to_string();
                            }
                            KeyCode::Char('s') => {
                                self.input_mode = AppInputMode::StoppedTyping;
                                self.is_listening_shared.store(false, Ordering::Relaxed);
                                self.status = "Stopped. Press 's' to Start. Type your message, Enter to process.".to_string();
                                self.current_live_japanese.clear(); // Clear live transcription
                                self.user_input.clear(); // Clear previous user input
                            }
                            _ => {}
                        },
                        AppInputMode::StoppedTyping => match key.code {
                            KeyCode::Char('q') => {
                                self.should_quit = true;
                                self.status = "Exiting...".to_string();
                            }
                            KeyCode::Char('s') => {
                                self.input_mode = AppInputMode::Listening;
                                self.is_listening_shared.store(true, Ordering::Relaxed);
                                self.status =
                                    "Starting... Press 's' to Stop/Start, 'q' to Quit".to_string();
                                self.user_input.clear();
                            }
                            KeyCode::Enter => {
                                // Process self.user_input (transcribe/translate)
                                // This part will require sending the user_input to the audio_processing_task
                                // or a similar new task. For now, we'll just clear it and log.
                                if !self.user_input.is_empty() {
                                    // Send user_input for processing. This needs a new AppUpdate variant or mechanism.
                                    // For now, let's simulate it goes to Japanese history.
                                    self.completed_japanese
                                        .push(format!("[User Input]: {}", self.user_input.clone()));
                                    // Add a placeholder for translation
                                    if self.completed_translations.len()
                                        < self.completed_japanese.len()
                                    {
                                        self.completed_translations
                                            .push("Translating user input...".to_string());
                                    }
                                    // Here you would ideally trigger a Llama translation for self.user_input
                                    self.status = format!(
                                        "Input '{}' submitted. Press 's' to start listening.",
                                        self.user_input
                                    );
                                    self.user_input.clear();
                                }
                            }
                            KeyCode::Char(c) => {
                                self.user_input.push(c);
                            }
                            KeyCode::Backspace => {
                                self.user_input.pop();
                            }
                            _ => {}
                        },
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_updates(&mut self) {
        while let Ok(update) = self.rx.try_recv() {
            self.on_update(update);
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        // Changed to &mut self
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![
                Constraint::Length(1), // Status
                Constraint::Length(3), // Live Japanese
                Constraint::Min(0),    // History
            ])
            .split(frame.area());

        // General Status/Help Message
        let help_text = match self.input_mode {
            AppInputMode::Listening => {
                format!(
                    "Status: {} ({} samples processed) (Press 's' to Stop, 'q' to Quit)",
                    self.status, self.total_samples_listened
                )
            }
            AppInputMode::StoppedTyping => {
                "Status: ".to_string()
                    + &self.status
                    + " (Press 's' to Start, 'q' to Quit, Enter to submit input)"
            }
        };
        let help_paragraph = Paragraph::new(help_text).style(Style::default().fg(Color::Yellow));
        frame.render_widget(help_paragraph, main_layout[0]);

        // Input Area (Live Japanese or User Text Input)
        let input_area_title = match self.input_mode {
            AppInputMode::Listening => "Live Japanese Input (Listening...)",
            AppInputMode::StoppedTyping => "Text Input (Stopped - Type here)",
        };
        let input_block = Block::default()
            .title(input_area_title)
            .borders(Borders::ALL);

        let text_to_display_in_input_area = match self.input_mode {
            AppInputMode::Listening => self.current_live_japanese.as_str(),
            AppInputMode::StoppedTyping => self.user_input.as_str(),
        };

        let mut text_widget = Paragraph::new(text_to_display_in_input_area)
            .wrap(Wrap { trim: true })
            .block(input_block.clone());

        if self.input_mode == AppInputMode::StoppedTyping {
            text_widget = text_widget.style(Style::default().fg(Color::Cyan)); // Style for typing mode
            // Set cursor position for typing mode
            #[allow(clippy::cast_possible_truncation)]
            frame.set_cursor_position(Position::new(
                main_layout[1].x + self.user_input.chars().count() as u16 + 1,
                main_layout[1].y + 1,
            ));
        } else if self.current_live_japanese.is_empty()
            && self.input_mode == AppInputMode::Listening
            && self.status.contains("Listening")
        {
            let listening_text = format!(
                "Listening... ({} samples processed this segment)",
                self.raw_samples_count
            );
            let listening_placeholder = Paragraph::new(listening_text)
                .wrap(Wrap { trim: true })
                .block(input_block)
                .style(Style::default().add_modifier(Modifier::ITALIC));
            frame.render_widget(listening_placeholder, main_layout[1]);
        } else {
            frame.render_widget(text_widget.clone(), main_layout[1]);
        }
        // If it's StoppedTyping mode, render text_widget again to ensure cursor is handled correctly
        // This is a bit redundant but ensures the cursor logic from above is effective
        // This is needed because we might have rendered the "Listening..." placeholder.
        if self.input_mode == AppInputMode::StoppedTyping {
            frame.render_widget(text_widget, main_layout[1]);
        }

        let history_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(main_layout[2]);

        // Japanese Transcript Panel
        let japanese_lines: Vec<Line> = self
            .completed_japanese
            .iter()
            .enumerate()
            .flat_map(|(i, s)| {
                let style = if i == 0 {
                    // Highlight the first line (newest)
                    Style::new().fg(Color::White)
                } else {
                    Style::new().fg(Color::DarkGray)
                };
                let content_line = Line::from(s.as_str()).style(style);
                if i == 0 {
                    // Newest item, don't add preceding blank line
                    vec![content_line]
                } else {
                    // Add a blank line before older items for separation
                    vec![Line::from(""), content_line]
                }
            })
            .collect();

        let japanese_block = Block::default()
            .title("Japanese Transcript")
            .borders(Borders::ALL);
        let common_wrap_setting = Wrap { trim: true };

        // Set scrollbar content length to number of items
        self.japanese_scroll_state = self
            .japanese_scroll_state
            .content_length(self.completed_japanese.len());

        // Auto-scroll logic removed. Scrolling is now manual.

        let japanese_paragraph = Paragraph::new(japanese_lines)
            .block(japanese_block)
            .wrap(common_wrap_setting)
            .scroll((self.japanese_scroll as u16, 0));
        frame.render_widget(japanese_paragraph, history_layout[0]);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("↑"))
                .end_symbol(Some("↓")),
            history_layout[0], // Render scrollbar in the same area
            &mut self.japanese_scroll_state,
        );

        // English Translation Panel
        let english_lines: Vec<Line> = self
            .completed_translations
            .iter()
            .enumerate()
            .flat_map(|(i, s)| {
                let style = if i == 0 {
                    // Highlight the first line (newest)
                    Style::new().fg(Color::White)
                } else {
                    Style::new().fg(Color::DarkGray)
                };
                let content_line = Line::from(s.as_str()).style(style);
                if i == 0 {
                    // Newest item
                    vec![content_line]
                } else {
                    // Add a blank line before older items
                    vec![Line::from(""), content_line]
                }
            })
            .collect();

        let english_block = Block::default()
            .title("English Translation")
            .borders(Borders::ALL);
        // Assuming same wrap setting as Japanese panel, can be customized if needed
        let common_wrap_setting = Wrap { trim: true };

        // Set scrollbar content length to number of items
        self.english_scroll_state = self
            .english_scroll_state
            .content_length(self.completed_translations.len());

        // Auto-scroll logic removed. Scrolling is now manual.

        let english_paragraph = Paragraph::new(english_lines)
            .block(english_block)
            .wrap(common_wrap_setting)
            .scroll((self.english_scroll, 0));
        frame.render_widget(english_paragraph, history_layout[1]);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("↑"))
                .end_symbol(Some("↓")),
            history_layout[1], // Render scrollbar in the same area
            &mut self.english_scroll_state,
        );
    }

    fn scroll_japanese_down(&mut self) {
        let content_height = self.completed_japanese.len();
        // Assuming roughly one line per item for simplicity in limiting scroll.
        // A more precise calculation might involve the actual rendered height if lines wrap.
        if content_height > 0 {
            // Check if there's content to scroll
            self.japanese_scroll = self.japanese_scroll.saturating_add(1);
            // Prevent scrolling beyond content. The paragraph widget itself might also clamp this.
            // This is a basic clamp; true max scroll depends on viewport height vs content height.
            if self.japanese_scroll >= content_height {
                self.japanese_scroll = content_height.saturating_sub(1);
            }
        }
        self.japanese_scroll_state = self.japanese_scroll_state.position(self.japanese_scroll);
    }

    fn scroll_japanese_up(&mut self) {
        self.japanese_scroll = self.japanese_scroll.saturating_sub(1);
        self.japanese_scroll_state = self.japanese_scroll_state.position(self.japanese_scroll);
    }

    fn scroll_english_down(&mut self) {
        let content_height = self.completed_translations.len() as u16;
        if content_height > 0 {
            self.english_scroll = self.english_scroll.saturating_add(1);
            if self.english_scroll >= content_height {
                self.english_scroll = content_height.saturating_sub(1);
            }
        }
        self.english_scroll_state = self
            .english_scroll_state
            .position(self.english_scroll as usize);
    }

    fn scroll_english_up(&mut self) {
        self.english_scroll = self.english_scroll.saturating_sub(1);
        self.english_scroll_state = self
            .english_scroll_state
            .position(self.english_scroll as usize);
    }
}

const SYSTEM_PROMPT: &str = "You are an expert translator. Translate the given Japanese text to English accurately and concisely. Output only the English translation. Do not add any pleasantries or extra explanations.";

async fn audio_processing_task(
    tx: mpsc::Sender<AppUpdate>,
    is_listening_shared: Arc<AtomicBool>,
) -> Result<(), anyhow::Error> {
    tx.send(AppUpdate::StatusUpdate(
        "Initializing models...".to_string(),
    ))
    .await
    .ok();

    let whisper_model = WhisperBuilder::default()
        .with_language(Some(WhisperLanguage::Japanese)) // Specify Japanese
        .build()
        .await?;

    tx.send(AppUpdate::StatusUpdate(
        "Whisper model loaded. Initializing Llama...".to_string(),
    ))
    .await
    .ok();

    let llama_model = Llama::builder()
        .with_source(LlamaSource::qwen_2_5_7b_instruct()) // Or another suitable model
        .build()
        .await?;
    let llama_chat_template = llama_model.chat().with_system_prompt(SYSTEM_PROMPT);

    tx.send(AppUpdate::StatusUpdate(
        "All models loaded. Listening for microphone input...".to_string(),
    ))
    .await
    .ok();

    let mic_input = MicInput::default();
    let vad_stream = mic_input.stream().voice_activity_stream();
    let tx_for_inspect = tx.clone(); // Clone tx for the inspect closure
    let mut audio_chunks = vad_stream
        .inspect(move |vad_output| {
            // vad_output is &VoiceActivityDetectorOutput (or the item type of vad_stream)
            // This assumes vad_output has a public field `samples` which is a `rodio::buffer::SamplesBuffer<f32>`
            // as per the user-provided reference.
            let samples_count = vad_output.samples.clone().count();
            if samples_count > 0 {
                // Use try_send to avoid blocking the audio thread.
                // If the channel is full or disconnected, this will be a no-op.
                tx_for_inspect
                    .try_send(AppUpdate::RawSamplesDetected(samples_count))
                    .ok();
            }
        })
        .rechunk_voice_activity()
        .with_end_window(std::time::Duration::from_millis(400)) // More sensitive end window
        .with_end_threshold(0.25) // Slightly higher end threshold
        .with_time_before_speech(std::time::Duration::from_millis(200)); // Reduce pre-speech buffer

    loop {
        if !is_listening_shared.load(Ordering::Relaxed) {
            // If not listening, sleep for a bit and check again.
            // Update status to indicate paused state if desired.
            // tx.send(AppUpdate::StatusUpdate("Audio processing paused...".to_string())).await.ok();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }

        // Check if there's an audio chunk available without blocking indefinitely if not listening
        // This might need more sophisticated handling if audio_chunks.next() blocks for too long
        // when is_listening_shared becomes false during its await.
        // For simplicity, we proceed with next().await.
        // A more robust solution might involve a select! with a shutdown signal.
        let input_audio_chunk = match tokio::time::timeout(
            std::time::Duration::from_millis(50), // Short timeout to remain responsive to is_listening_shared
            audio_chunks.next(),
        )
        .await
        {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,  // Stream ended
            Err(_) => continue, // Timeout, loop back to check is_listening_shared
        };

        // Indicate that an audio chunk has been received and provide its size
        let chunk_size = input_audio_chunk.clone().count(); // Get number of samples directly from SamplesBuffer
        tx.send(AppUpdate::StatusUpdate(format!(
            "Processing audio chunk ({:#?} samples)...",
            chunk_size
        )))
        .await
        .ok();
        // Send the number of samples processed in this chunk for this segment
        tx.send(AppUpdate::SamplesProcessed(chunk_size)).await.ok();

        // tx.send(AppUpdate::StatusUpdate("Transcribing audio...".to_string()))
        //     .await
        //     .ok(); // This line is now replaced by the more specific one above or the one below after transcription
        let mut current_segment_text = String::new();
        let mut transcribed_stream = whisper_model.transcribe(input_audio_chunk);

        while let Some(transcribed) = transcribed_stream.next().await {
            if transcribed.probability_of_no_speech() < 0.85 {
                current_segment_text.push_str(transcribed.text());
                tx.send(AppUpdate::LiveJapaneseUpdate(current_segment_text.clone()))
                    .await
                    .ok();
            }
        }

        if current_segment_text.trim().chars().count() > 0 {
            tx.send(AppUpdate::JapaneseSegmentComplete(
                current_segment_text.clone(),
            ))
            .await
            .ok();
            tx.send(AppUpdate::StatusUpdate(
                "Translating to English...".to_string(),
            ))
            .await
            .ok();

            let tx_clone_for_task = tx.clone();
            let chat_template_for_task = llama_chat_template.clone();
            let segment_to_translate = current_segment_text.clone();

            tokio::spawn(async move {
                let prompt = format!(
                    "Translate the following Japanese text to English, Output only the English translation. Do not add any pleasantries or extra explanations. Do not translate English, keep as is.:\n{}",
                    segment_to_translate
                );

                // It's good practice to indicate that the Llama call is starting within the task
                // tx_clone_for_task.send(AppUpdate::StatusUpdate(
                //     "Requesting translation from Llama...".to_string(),
                // ))
                // .await
                // .ok();

                let mut llama_chat = chat_template_for_task;
                let mut response_stream = llama_chat(&prompt);
                let raw_translation = response_stream.all_text().await;
                // println!("[Debug Llama Output Live]: {}", raw_translation);

                let cleaned_translation = raw_translation
                    .replace("<|im_start|>", "")
                    .replace("<|im_end|>", "")
                    .trim()
                    .to_string();

                let _status_translation_excerpt = if cleaned_translation.len() > 20 {
                    let mut end_index = 20;
                    if cleaned_translation.is_empty() {
                        end_index = 0;
                    } else {
                        while end_index > 0 && !cleaned_translation.is_char_boundary(end_index) {
                            end_index -= 1;
                        }
                    }
                    format!("{}...", &cleaned_translation[..end_index])
                } else {
                    cleaned_translation.clone()
                };
                // This status update can be useful to confirm the task completed
                // tx_clone_for_task.send(AppUpdate::StatusUpdate(format!(
                //     "Llama call completed. Got: {}",
                //     status_translation_excerpt
                // )))
                // .await
                // .ok();

                if !cleaned_translation.is_empty() {
                    tx_clone_for_task
                        .send(AppUpdate::EnglishTranslation(cleaned_translation))
                        .await
                        .ok();
                } else {
                    tx_clone_for_task
                        .send(AppUpdate::EnglishTranslation(
                            "[No translation generated]".to_string(),
                        ))
                        .await
                        .ok();
                }
            });
        } else {
            // Clear live japanese if segment was too short/empty
            tx.send(AppUpdate::LiveJapaneseUpdate("".to_string()))
                .await
                .ok();
        }
        tx.send(AppUpdate::StatusUpdate("Listening...".to_string()))
            .await
            .ok();
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let (tx, rx) = mpsc::channel(32); // Channel for AppUpdates
    let is_listening_shared = Arc::new(AtomicBool::new(true)); // Initially listening

    // Clone tx and is_listening_shared for the audio processing task
    let tx_audio = tx.clone();
    let is_listening_audio_task = is_listening_shared.clone();
    tokio::spawn(async move {
        if let Err(e) = audio_processing_task(tx_audio, is_listening_audio_task).await {
            // Send error to UI if task fails
            // The tx channel might be closed if the main app loop has already exited.
            // We use a let _ to ignore the result of the send, as there's not much we can do
            // if sending fails here (the UI part is likely gone).
            let _ = tx
                .send(AppUpdate::Error(format!(
                    "Audio processing task failed: {}",
                    e
                )))
                .await;
        }
    });

    // Setup terminal
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture // Though not used, good practice
    )?;
    terminal.clear()?; // Clear terminal before first draw

    let mut app = App::new(rx); // app needs to be mutable to call run
    let app_result = app.run(&mut terminal); // Pass a mutable reference to terminal

    // Restore terminal
    crossterm::execute!(
        terminal.backend_mut(), // Use terminal.backend_mut() for restore as well
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    crossterm::terminal::disable_raw_mode()?;

    // The terminal is dropped here, which should restore the original screen.
    // Explicitly restoring is good practice though.

    if let Err(err) = app_result {
        // It's good to print the error to stderr if the application fails
        // before the terminal is fully restored, or if restoration itself fails.
        eprintln!("Error running app: {:?}", err);
        // Ensure color_eyre's hook can run by returning the error
        return Err(err); // No need for .into() if app.run returns color_eyre::Result
    }

    Ok(())
}

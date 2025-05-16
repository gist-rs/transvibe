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
    SamplesProcessed(usize), // Added to track processed samples
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
    english_scroll: usize,
    total_samples_listened: usize, // Added to accumulate listened samples
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
            total_samples_listened: 0, // Initialize listened samples
        }
    }

    fn on_update(&mut self, update: AppUpdate) {
        match update {
            AppUpdate::StatusUpdate(s) => self.status = s,
            AppUpdate::LiveJapaneseUpdate(s) => self.current_live_japanese = s,
            AppUpdate::JapaneseSegmentComplete(jp_text) => {
                self.completed_japanese.push(jp_text);
                self.current_live_japanese.clear();
                // Keep lists of the same length, add placeholder if translation is pending
                if self.completed_translations.len() < self.completed_japanese.len() {
                    self.completed_translations
                        .push("Translating...".to_string());
                }
            }
            AppUpdate::EnglishTranslation(en_text) => {
                let jp_len = self.completed_japanese.len();
                let tr_len = self.completed_translations.len();

                // Ideal case: translation corresponds to the last Japanese segment for which a placeholder exists.
                if tr_len > 0
                    && tr_len == jp_len
                    && self.completed_translations[tr_len - 1] == "Translating..."
                {
                    self.completed_translations[tr_len - 1] = en_text;
                }
                // Fallback: find the most recent "Translating..." placeholder and update it.
                else if let Some(index) = self
                    .completed_translations
                    .iter()
                    .rposition(|t| t == "Translating...")
                {
                    self.completed_translations[index] = en_text;
                }
                // Further fallback: if lengths allow, append new translation.
                else if tr_len < jp_len {
                    // This implies a Japanese text exists, but no "Translating..." placeholder was found for it.
                    // We'll append the translation, assuming it's for the latest segment.
                    self.completed_translations.push(en_text);
                }
                // If none of the above, the translation might be an anomaly.
                // For now, we let the cleanup logic below adjust list lengths.

                // Ensure translation list doesn't grow longer than Japanese list.
                while self.completed_translations.len() > self.completed_japanese.len() {
                    self.completed_translations.pop();
                }
                // Ensure there's a placeholder if a Japanese text exists without a corresponding translation/placeholder.
                // This is defensive, in case the "Translating..." wasn't added or was prematurely removed.
                while self.completed_translations.len() < self.completed_japanese.len() {
                    self.completed_translations
                        .push("[Pending Translation]".to_string());
                }
            }
            AppUpdate::SamplesProcessed(samples) => {
                self.total_samples_listened += samples;
            }
            AppUpdate::Error(e) => {
                self.status = format!("ERROR: {}", e);
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
            frame.set_cursor(
                main_layout[1].x + self.user_input.chars().count() as u16 + 1,
                main_layout[1].y + 1,
            );
        } else if self.current_live_japanese.is_empty()
            && self.input_mode == AppInputMode::Listening
            && self.status.contains("Listening")
        {
            let listening_placeholder = Paragraph::new("Listening...")
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
            .map(|s| Line::from(s.as_str()))
            .collect();
        let japanese_text_height = japanese_lines.len();

        self.japanese_scroll_state = self
            .japanese_scroll_state
            .content_length(japanese_text_height);

        let japanese_paragraph = Paragraph::new(japanese_lines)
            .block(
                Block::default()
                    .title("Japanese Transcript")
                    .borders(Borders::ALL),
            )
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: true }) // Trim false might be better for precise scrolling
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
            .map(|s| Line::from(s.as_str()))
            .collect();
        let english_text_height = english_lines.len();

        self.english_scroll_state = self
            .english_scroll_state
            .content_length(english_text_height);

        let english_paragraph = Paragraph::new(english_lines)
            .block(
                Block::default()
                    .title("English Translation")
                    .borders(Borders::ALL),
            )
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: true })
            .scroll((self.english_scroll as u16, 0));
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
        let content_height = self.completed_translations.len();
        if content_height > 0 {
            self.english_scroll = self.english_scroll.saturating_add(1);
            if self.english_scroll >= content_height {
                self.english_scroll = content_height.saturating_sub(1);
            }
        }
        self.english_scroll_state = self.english_scroll_state.position(self.english_scroll);
    }

    fn scroll_english_up(&mut self) {
        self.english_scroll = self.english_scroll.saturating_sub(1);
        self.english_scroll_state = self.english_scroll_state.position(self.english_scroll);
    }
}

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
    let mut llama_chat = llama_model.chat().with_system_prompt("You are an expert translator. Translate the given Japanese text to English accurately and concisely. Output only the English translation. Do not add any pleasantries or extra explanations.");

    tx.send(AppUpdate::StatusUpdate(
        "All models loaded. Listening for microphone input...".to_string(),
    ))
    .await
    .ok();

    let mic_input = MicInput::default();
    let mut audio_chunks = mic_input
        .stream()
        .voice_activity_stream()
        .rechunk_voice_activity();

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
        let chunk_size = input_audio_chunk.size_hint().0; // Get number of samples from iterator's size_hint
        tx.send(AppUpdate::StatusUpdate(format!(
            "Processing audio chunk ({} samples)...",
            chunk_size
        )))
        .await
        .ok();
        // Send the number of samples processed in this chunk
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

        if current_segment_text.trim().chars().count() > 1 {
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

            tx.send(AppUpdate::StatusUpdate(
                "Requesting translation from Llama...".to_string(),
            ))
            .await
            .ok();
            let prompt = format!(
                "Translate the following Japanese text to English, Output only the English translation. Do not add any pleasantries or extra explanations: '{}'",
                current_segment_text
            );
            let mut response_stream = llama_chat(&prompt);
            let translation = response_stream.all_text().await; // Use all_text()
            // println!("[Debug Llama Output Live]: {}", translation); // Clarified log source

            let status_translation_excerpt = if translation.len() > 20 {
                let mut end_index = 20;
                // Ensure we don't panic if the string is shorter than 20 bytes after all,
                // or if the loop somehow goes below zero (though it shouldn't with valid UTF-8).
                // We also need to check if the string is empty to avoid panicking on is_char_boundary(0) for an empty string.
                if translation.is_empty() {
                    end_index = 0;
                } else {
                    while end_index > 0 && !translation.is_char_boundary(end_index) {
                        end_index -= 1;
                    }
                }
                format!("{}...", &translation[..end_index])
            } else {
                translation.clone()
            };
            tx.send(AppUpdate::StatusUpdate(format!(
                "Llama call completed. Got: {}",
                status_translation_excerpt
            )))
            .await
            .ok();

            if !translation.is_empty() {
                tx.send(AppUpdate::EnglishTranslation(translation))
                    .await
                    .ok();
            } else {
                // Handle cases where translation might be empty
                tx.send(AppUpdate::EnglishTranslation(
                    "[No translation generated]".to_string(),
                ))
                .await
                .ok();
            }
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

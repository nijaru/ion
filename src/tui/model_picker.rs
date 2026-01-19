//! Two-stage model picker: Provider → Model selection.

use crate::provider::{ModelFilter, ModelInfo, ModelRegistry, ProviderPrefs};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use std::collections::BTreeMap;

/// Selection stage for the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerStage {
    Provider,
    Model,
}

/// Provider with aggregated stats.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub name: String,
    pub model_count: usize,
    pub min_price: f64,
    pub has_cache: bool,
}

/// State for the two-stage model picker modal.
pub struct ModelPicker {
    /// Current selection stage.
    pub stage: PickerStage,
    /// All available models (fetched from registry).
    pub all_models: Vec<ModelInfo>,
    /// Models grouped by provider.
    pub providers: Vec<ProviderEntry>,
    /// Filtered providers based on search.
    pub filtered_providers: Vec<ProviderEntry>,
    /// Models for selected provider.
    pub provider_models: Vec<ModelInfo>,
    /// Filtered models based on search.
    pub filtered_models: Vec<ModelInfo>,
    /// Current filter text.
    pub filter: String,
    /// Provider list state.
    pub provider_state: ListState,
    /// Model list state.
    pub model_state: ListState,
    /// Selected provider name.
    pub selected_provider: Option<String>,
    /// Provider preferences for filtering.
    pub prefs: ProviderPrefs,
    /// Loading state.
    pub is_loading: bool,
    /// Error message if fetch failed.
    pub error: Option<String>,
    /// Current API provider name (e.g., "OpenRouter", "Ollama").
    pub api_provider_name: Option<String>,
}

impl Default for ModelPicker {
    fn default() -> Self {
        Self {
            stage: PickerStage::Provider,
            all_models: Vec::new(),
            providers: Vec::new(),
            filtered_providers: Vec::new(),
            provider_models: Vec::new(),
            filtered_models: Vec::new(),
            filter: String::new(),
            provider_state: ListState::default(),
            model_state: ListState::default(),
            selected_provider: None,
            prefs: ProviderPrefs::default(),
            is_loading: false,
            error: None,
            api_provider_name: None,
        }
    }
}

impl ModelPicker {
    pub fn new(prefs: ProviderPrefs) -> Self {
        Self {
            prefs,
            ..Default::default()
        }
    }

    /// Reset picker to provider stage (for Ctrl+P: Provider → Model flow).
    pub fn reset(&mut self) {
        self.stage = PickerStage::Provider;
        self.filter.clear();
        self.selected_provider = None;
        self.provider_models.clear();
        self.filtered_models.clear();
        self.model_state.select(None);
        self.apply_provider_filter();
    }

    /// Start picker directly at model stage for a specific provider (for Ctrl+M: Model only).
    pub fn start_model_only(&mut self, provider: &str) {
        self.selected_provider = Some(provider.to_string());
        self.provider_models = self
            .all_models
            .iter()
            .filter(|m| m.provider == provider)
            .cloned()
            .collect();
        self.stage = PickerStage::Model;
        self.filter.clear();
        self.apply_model_filter();
    }

    /// Start picker showing all models (no provider filter).
    pub fn start_all_models(&mut self) {
        self.selected_provider = None;
        self.provider_models = self.all_models.clone();
        // Sorting handled by apply_model_filter()
        self.stage = PickerStage::Model;
        self.filter.clear();
        self.apply_model_filter();
    }

    /// Check if we have models loaded.
    pub fn has_models(&self) -> bool {
        !self.all_models.is_empty()
    }

    /// Set the API provider name (e.g., "OpenRouter", "Ollama").
    pub fn set_api_provider(&mut self, name: impl Into<String>) {
        self.api_provider_name = Some(name.into());
    }

    /// Set models from registry and build provider list.
    pub fn set_models(&mut self, models: Vec<ModelInfo>) {
        self.all_models = models;
        self.build_provider_list();
        self.apply_provider_filter();
        self.is_loading = false;
    }

    /// Set error state.
    pub fn set_error(&mut self, err: String) {
        self.error = Some(err);
        self.is_loading = false;
    }

    /// Build aggregated provider list from models.
    fn build_provider_list(&mut self) {
        let mut by_provider: BTreeMap<String, Vec<&ModelInfo>> = BTreeMap::new();

        for model in &self.all_models {
            by_provider
                .entry(model.provider.clone())
                .or_default()
                .push(model);
        }

        self.providers = by_provider
            .into_iter()
            .map(|(name, models)| {
                let min_price = models
                    .iter()
                    .map(|m| m.pricing.input)
                    .fold(f64::INFINITY, f64::min);
                let has_cache = models.iter().any(|m| m.supports_cache);

                ProviderEntry {
                    name,
                    model_count: models.len(),
                    min_price: if min_price.is_infinite() {
                        0.0
                    } else {
                        min_price
                    },
                    has_cache,
                }
            })
            .collect();

        // Sort: cache-supporting first if preferred, then by min price
        if self.prefs.prefer_cache {
            self.providers
                .sort_by(|a, b| match b.has_cache.cmp(&a.has_cache) {
                    std::cmp::Ordering::Equal => a
                        .min_price
                        .partial_cmp(&b.min_price)
                        .unwrap_or(std::cmp::Ordering::Equal),
                    other => other,
                });
        } else {
            self.providers.sort_by(|a, b| {
                a.min_price
                    .partial_cmp(&b.min_price)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }

    /// Apply filter to provider list.
    fn apply_provider_filter(&mut self) {
        let filter_lower = self.filter.to_lowercase();

        self.filtered_providers = self
            .providers
            .iter()
            .filter(|p| {
                if filter_lower.is_empty() {
                    return true;
                }
                p.name.to_lowercase().contains(&filter_lower)
            })
            .cloned()
            .collect();

        if !self.filtered_providers.is_empty() {
            self.provider_state.select(Some(0));
        } else {
            self.provider_state.select(None);
        }
    }

    /// Apply filter to model list.
    fn apply_model_filter(&mut self) {
        let filter_lower = self.filter.to_lowercase();

        self.filtered_models = self
            .provider_models
            .iter()
            .filter(|m| {
                if filter_lower.is_empty() {
                    return true;
                }
                m.id.to_lowercase().contains(&filter_lower)
                    || m.name.to_lowercase().contains(&filter_lower)
            })
            .cloned()
            .collect();

        // Sort: org first, then newest first (by created timestamp descending)
        self.filtered_models.sort_by(|a, b| {
            // Primary: org name
            a.provider.cmp(&b.provider).then_with(|| {
                // Secondary: newest first (higher created = newer)
                b.created.cmp(&a.created)
            })
        });

        if !self.filtered_models.is_empty() {
            self.model_state.select(Some(0));
        } else {
            self.model_state.select(None);
        }
    }

    /// Select current provider and move to model stage.
    pub fn select_provider(&mut self) {
        if let Some(idx) = self.provider_state.selected() {
            if let Some(provider) = self.filtered_providers.get(idx) {
                self.selected_provider = Some(provider.name.clone());
                self.provider_models = self
                    .all_models
                    .iter()
                    .filter(|m| m.provider == provider.name)
                    .cloned()
                    .collect();
                self.stage = PickerStage::Model;
                self.filter.clear();
                self.apply_model_filter();
            }
        }
    }

    /// Go back to provider stage.
    pub fn back_to_providers(&mut self) {
        self.stage = PickerStage::Provider;
        self.filter.clear();
        self.selected_provider = None;
        self.apply_provider_filter();
    }

    /// Add character to filter.
    pub fn push_char(&mut self, c: char) {
        self.filter.push(c);
        match self.stage {
            PickerStage::Provider => self.apply_provider_filter(),
            PickerStage::Model => self.apply_model_filter(),
        }
    }

    /// Remove last character from filter.
    pub fn pop_char(&mut self) {
        self.filter.pop();
        match self.stage {
            PickerStage::Provider => self.apply_provider_filter(),
            PickerStage::Model => self.apply_model_filter(),
        }
    }

    /// Move selection up.
    pub fn move_up(&mut self, count: usize) {
        match self.stage {
            PickerStage::Provider => {
                if self.filtered_providers.is_empty() {
                    return;
                }
                let i = self.provider_state.selected().unwrap_or(0);
                let new_i = i.saturating_sub(count);
                self.provider_state.select(Some(new_i));
            }
            PickerStage::Model => {
                if self.filtered_models.is_empty() {
                    return;
                }
                let i = self.model_state.selected().unwrap_or(0);
                let new_i = i.saturating_sub(count);
                self.model_state.select(Some(new_i));
            }
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self, count: usize) {
        match self.stage {
            PickerStage::Provider => {
                if self.filtered_providers.is_empty() {
                    return;
                }
                let len = self.filtered_providers.len();
                let i = self.provider_state.selected().unwrap_or(0);
                let new_i = (i + count).min(len - 1);
                self.provider_state.select(Some(new_i));
            }
            PickerStage::Model => {
                if self.filtered_models.is_empty() {
                    return;
                }
                let len = self.filtered_models.len();
                let i = self.model_state.selected().unwrap_or(0);
                let new_i = (i + count).min(len - 1);
                self.model_state.select(Some(new_i));
            }
        }
    }

    /// Jump to top.
    pub fn jump_to_top(&mut self) {
        match self.stage {
            PickerStage::Provider => {
                if !self.filtered_providers.is_empty() {
                    self.provider_state.select(Some(0));
                }
            }
            PickerStage::Model => {
                if !self.filtered_models.is_empty() {
                    self.model_state.select(Some(0));
                }
            }
        }
    }

    /// Jump to bottom.
    pub fn jump_to_bottom(&mut self) {
        match self.stage {
            PickerStage::Provider => {
                if !self.filtered_providers.is_empty() {
                    self.provider_state
                        .select(Some(self.filtered_providers.len() - 1));
                }
            }
            PickerStage::Model => {
                if !self.filtered_models.is_empty() {
                    self.model_state
                        .select(Some(self.filtered_models.len() - 1));
                }
            }
        }
    }

    /// Get currently selected model (only valid in Model stage).
    pub fn selected_model(&self) -> Option<&ModelInfo> {
        if self.stage != PickerStage::Model {
            return None;
        }
        self.model_state
            .selected()
            .and_then(|i| self.filtered_models.get(i))
    }

    /// Render the picker modal.
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Modal width for columns: name(36) + provider(20) + context(7) + price(9) + borders/spacing
        let content_width = 80u16;
        let list_len = match self.stage {
            PickerStage::Provider => self.filtered_providers.len(),
            PickerStage::Model => self.filtered_models.len(),
        };
        let list_height = (list_len as u16 + 2).clamp(5, 30); // +2 for borders, min 5, max 30
        let total_height = 3 + list_height; // search bar + list

        let modal_width = content_width.min(area.width.saturating_sub(4));
        let modal_height = total_height.min(area.height.saturating_sub(4));
        let x = (area.width - modal_width) / 2;
        let y = (area.height - modal_height) / 2;
        let modal_area = Rect::new(x, y, modal_width, modal_height);

        // Clear the background
        frame.render_widget(Clear, modal_area);

        // Split into search bar + list
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(modal_area);

        // Search input with API provider name
        let search_title = match self.stage {
            PickerStage::Provider => " Filter (type to search) ".to_string(),
            PickerStage::Model => {
                if let Some(ref name) = self.api_provider_name {
                    format!(" {} - Filter (type to search) ", name)
                } else {
                    " Filter (type to search) ".to_string()
                }
            }
        };

        let search_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(search_title);

        let search_text = if self.filter.is_empty() {
            " ...".to_string()
        } else {
            format!(" {}_", self.filter)
        };

        let search_style = if self.filter.is_empty() {
            Style::default().dim()
        } else {
            Style::default().fg(Color::Yellow)
        };

        let search_para = Paragraph::new(search_text)
            .style(search_style)
            .block(search_block);
        frame.render_widget(search_para, chunks[0]);

        // Loading/Error state
        if self.is_loading {
            let provider_name = self.api_provider_name.as_deref().unwrap_or("provider");
            let loading = Paragraph::new(format!("Loading models from {}...", provider_name))
                .style(Style::default().fg(Color::Yellow))
                .block(Block::default().borders(Borders::ALL).title(" Loading "));
            frame.render_widget(loading, chunks[1]);
            return;
        }

        if let Some(ref err) = self.error {
            let error = Paragraph::new(format!("Error: {}", err))
                .style(Style::default().fg(Color::Red))
                .block(Block::default().borders(Borders::ALL).title(" Error "));
            frame.render_widget(error, chunks[1]);
            return;
        }

        match self.stage {
            PickerStage::Provider => self.render_provider_list(frame, chunks[1]),
            PickerStage::Model => {
                // Split area for header + list
                let model_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Min(0)])
                    .split(chunks[1]);
                self.render_model_header(frame, model_chunks[0]);
                self.render_model_list(frame, model_chunks[1]);
            }
        }
    }

    fn render_model_header(&self, frame: &mut Frame, area: Rect) {
        // Column widths (must match render_model_list)
        let name_width = 38usize;
        let provider_width = 12usize;
        let context_width = 7usize;
        let input_width = 7usize;
        let output_width = 7usize;

        let header_line = Line::from(vec![
            Span::raw("  "), // Space for highlight symbol
            Span::styled(format!("{:width$}", "Model", width = name_width), Style::default().fg(Color::Cyan).bold()),
            Span::styled(format!("{:width$}", "Org", width = provider_width), Style::default().fg(Color::Cyan).bold()),
            Span::styled(format!("{:>width$}", "Context", width = context_width), Style::default().fg(Color::Cyan).bold()),
            Span::styled(format!("{:>width$}", "Input", width = input_width), Style::default().fg(Color::Cyan).bold()),
            Span::styled(format!("{:>width$}", "Output", width = output_width), Style::default().fg(Color::Cyan).bold()),
        ]);

        frame.render_widget(Paragraph::new(header_line), area);
    }

    fn render_provider_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .filtered_providers
            .iter()
            .map(|p| {
                let cache_indicator = if p.has_cache { "◆" } else { "○" };
                let price = format!("from ${:.2}/M", p.min_price);

                let line = Line::from(vec![
                    Span::styled(
                        cache_indicator,
                        if p.has_cache {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default().dim()
                        },
                    ),
                    Span::raw(" "),
                    Span::styled(&p.name, Style::default().fg(Color::White).bold()),
                    Span::raw(" "),
                    Span::styled(
                        format!("({} models)", p.model_count),
                        Style::default().fg(Color::Blue).dim(),
                    ),
                    Span::raw(" "),
                    Span::styled(price, Style::default().fg(Color::Yellow).dim()),
                ]);

                ListItem::new(line)
            })
            .collect();

        let count = self.filtered_providers.len();
        let total = self.providers.len();
        let title = format!(" Providers ({}/{}) ", count, total);

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(title),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");

        frame.render_stateful_widget(list, area, &mut self.provider_state);
    }

    fn render_model_list(&mut self, frame: &mut Frame, area: Rect) {
        // Column widths (must match render_model_header)
        let name_width = 38usize;
        let provider_width = 12usize;
        let context_width = 7usize;
        let input_width = 7usize;
        let output_width = 7usize;

        let items: Vec<ListItem> = self.filtered_models.iter().map(|m| {
            // Extract display name based on provider format
            let model_name = if m.provider == "ollama" {
                // Ollama: strip ":latest" suffix (it's the default)
                m.id.strip_suffix(":latest").unwrap_or(&m.id)
            } else {
                // Others: strip "provider/" prefix
                m.id.split('/').nth(1).unwrap_or(&m.id)
            };
            let provider = &m.provider;

            // Truncate if needed (accounting for ellipsis)
            let name_display: String = if model_name.chars().count() > name_width {
                let truncated: String = model_name.chars().take(name_width - 1).collect();
                format!("{}…", truncated)
            } else {
                model_name.to_string()
            };
            let provider_display: String = if provider.chars().count() > provider_width {
                let truncated: String = provider.chars().take(provider_width - 1).collect();
                format!("{}…", truncated)
            } else {
                provider.to_string()
            };

            // Format columns with padding
            let name_col = format!("{:width$}", name_display, width = name_width);
            let provider_col = format!("{:width$}", provider_display, width = provider_width);
            let context_col = format!("{:>width$}", format!("{}k", m.context_window / 1000), width = context_width);

            // Format prices - free models show "free", others show price
            let (input_str, input_style) = if m.pricing.input == 0.0 {
                ("free".to_string(), Style::default().fg(Color::Green))
            } else {
                (format!("${:.2}", m.pricing.input), Style::default().fg(Color::Yellow))
            };
            let (output_str, output_style) = if m.pricing.output == 0.0 {
                ("free".to_string(), Style::default().fg(Color::Green))
            } else {
                (format!("${:.2}", m.pricing.output), Style::default().fg(Color::Yellow))
            };
            let input_col = format!("{:>width$}", input_str, width = input_width);
            let output_col = format!("{:>width$}", output_str, width = output_width);

            let line = Line::from(vec![
                Span::styled(name_col, Style::default().fg(Color::White)),
                Span::styled(provider_col, Style::default().dim()),
                Span::styled(context_col, Style::default().fg(Color::Blue)),
                Span::styled(input_col, input_style),
                Span::styled(output_col, output_style),
            ]);

            ListItem::new(line)
        }).collect();

        let count = self.filtered_models.len();
        let total = self.provider_models.len();
        let title = match &self.selected_provider {
            Some(p) => format!(" {} ({}/{}) ", p, count, total),
            None => format!(" Models ({}/{}) ", count, total),
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(title),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");

        frame.render_stateful_widget(list, area, &mut self.model_state);
    }
}

use crate::provider::Backend;

/// Fetch models from registry for the given backend.
pub async fn fetch_models_for_picker(
    registry: &ModelRegistry,
    backend: Backend,
    prefs: &ProviderPrefs,
) -> Result<Vec<ModelInfo>, anyhow::Error> {
    let models = registry.fetch_models_for_backend(backend).await?;
    let filter = ModelFilter::default();
    Ok(registry.list_models_from_vec(models, &filter, prefs))
}

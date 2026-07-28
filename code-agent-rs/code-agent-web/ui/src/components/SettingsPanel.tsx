import { useState, useCallback, useEffect } from 'react';

// ── Storage Keys ───────────────────────────────────────────────────────────
const STORAGE_KEY = 'code_agent_settings';

// ── Types ──────────────────────────────────────────────────────────────────
export interface AgentSettings {
  model: string;
  temperature: number;
  maxIterations: number;
  systemInstructions: string;
}

const DEFAULTS: AgentSettings = {
  model: '',
  temperature: 0.7,
  maxIterations: 20,
  systemInstructions: 'You are a helpful coding assistant with access to file, git, shell, and search tools. Be concise and accurate.',
};

// ── Persistence ────────────────────────────────────────────────────────────
export function loadSettings(): AgentSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<AgentSettings>;
      return { ...DEFAULTS, ...parsed };
    }
  } catch {
    // ignore parse errors
  }
  return { ...DEFAULTS };
}

export function saveSettings(settings: AgentSettings): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
}

// ── Props ──────────────────────────────────────────────────────────────────
interface SettingsPanelProps {
  open: boolean;
  settings: AgentSettings;
  onSave: (settings: AgentSettings) => void;
  onClose: () => void;
}

// ── Component ───────────────────────────────────────────────────────────────
export default function SettingsPanel({ open, settings, onSave, onClose }: SettingsPanelProps) {
  const [local, setLocal] = useState<AgentSettings>(settings);

  useEffect(() => {
    setLocal(settings);
  }, [settings, open]);

  const handleChange = useCallback(<K extends keyof AgentSettings>(key: K, value: AgentSettings[K]) => {
    setLocal((prev) => ({ ...prev, [key]: value }));
  }, []);

  const handleSave = useCallback(() => {
    onSave(local);
    onClose();
  }, [local, onSave, onClose]);

  const handleReset = useCallback(() => {
    setLocal({ ...DEFAULTS });
  }, []);

  // Close on Escape key
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div id="settings-overlay" onClick={onClose}>
      <div id="settings-panel" onClick={(e) => e.stopPropagation()}>
        <div id="settings-header">
          <h2>Settings</h2>
          <button id="settings-close" onClick={onClose} title="Close">&times;</button>
        </div>

        <div id="settings-body">
          {/* Model */}
          <div className="settings-field">
            <label className="settings-label">Model</label>
            <input
              className="settings-input"
              type="text"
              value={local.model}
              onChange={(e) => handleChange('model', e.target.value)}
              placeholder="e.g. gpt-4o, qwen3.6-27b"
            />
            <span className="settings-hint">Leave empty to use server default</span>
          </div>

          {/* Temperature */}
          <div className="settings-field">
            <label className="settings-label">
              Temperature: <strong>{local.temperature.toFixed(1)}</strong>
            </label>
            <input
              className="settings-slider"
              type="range"
              min="0"
              max="2"
              step="0.1"
              value={local.temperature}
              onChange={(e) => handleChange('temperature', parseFloat(e.target.value))}
            />
            <div className="settings-slider-labels">
              <span>0 (precise)</span>
              <span>2 (creative)</span>
            </div>
          </div>

          {/* Max Iterations */}
          <div className="settings-field">
            <label className="settings-label">Max Iterations</label>
            <input
              className="settings-input settings-input-narrow"
              type="number"
              min="1"
              max="100"
              value={local.maxIterations}
              onChange={(e) => handleChange('maxIterations', Math.max(1, Math.min(100, parseInt(e.target.value, 10) || 1)))}
            />
          </div>

          {/* System Instructions */}
          <div className="settings-field">
            <label className="settings-label">System Instructions</label>
            <textarea
              className="settings-textarea"
              rows={4}
              value={local.systemInstructions}
              onChange={(e) => handleChange('systemInstructions', e.target.value)}
            />
          </div>
        </div>

        <div id="settings-footer">
          <button id="settings-reset-btn" onClick={handleReset}>Reset to Defaults</button>
          <div id="settings-footer-right">
            <button id="settings-cancel-btn" onClick={onClose}>Cancel</button>
            <button id="settings-save-btn" onClick={handleSave}>Save</button>
          </div>
        </div>
      </div>
    </div>
  );
}

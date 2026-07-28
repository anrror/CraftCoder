import { render, screen, fireEvent } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import SettingsPanel from '../components/SettingsPanel';
import type { AgentSettings } from '../components/SettingsPanel';

const defaultSettings: AgentSettings = {
  model: 'gpt-4o',
  temperature: 0.7,
  maxIterations: 20,
  systemInstructions: 'You are a helpful coding assistant.',
};

describe('SettingsPanel', () => {
  it('renders nothing when closed', () => {
    const { container } = render(
      <SettingsPanel open={false} settings={defaultSettings} onSave={vi.fn()} onClose={vi.fn()} />,
    );
    expect(container.innerHTML).toBe('');
  });

  it('renders modal when open', () => {
    render(
      <SettingsPanel open={true} settings={defaultSettings} onSave={vi.fn()} onClose={vi.fn()} />,
    );
    expect(screen.getByText('Settings')).toBeInTheDocument();
  });

  it('displays model value', () => {
    render(
      <SettingsPanel open={true} settings={defaultSettings} onSave={vi.fn()} onClose={vi.fn()} />,
    );
    const modelInput = screen.getByPlaceholderText('e.g. gpt-4o, qwen3.6-27b');
    expect(modelInput).toHaveValue('gpt-4o');
  });

  it('displays temperature value', () => {
    render(
      <SettingsPanel open={true} settings={defaultSettings} onSave={vi.fn()} onClose={vi.fn()} />,
    );
    expect(screen.getByText(/0.7/)).toBeInTheDocument();
  });

  it('displays max iterations', () => {
    render(
      <SettingsPanel open={true} settings={defaultSettings} onSave={vi.fn()} onClose={vi.fn()} />,
    );
    expect(screen.getByDisplayValue('20')).toBeInTheDocument();
  });

  it('calls onSave with updated settings', () => {
    const onSave = vi.fn();
    const onClose = vi.fn();
    render(
      <SettingsPanel open={true} settings={defaultSettings} onSave={onSave} onClose={onClose} />,
    );

    const modelInput = screen.getByPlaceholderText('e.g. gpt-4o, qwen3.6-27b');
    fireEvent.change(modelInput, { target: { value: 'qwen3.6-27b' } });

    fireEvent.click(screen.getByText('Save'));

    expect(onSave).toHaveBeenCalledWith(
      expect.objectContaining({ model: 'qwen3.6-27b' }),
    );
  });

  it('calls onClose when Cancel is clicked', () => {
    const onClose = vi.fn();
    render(
      <SettingsPanel open={true} settings={defaultSettings} onSave={vi.fn()} onClose={onClose} />,
    );

    fireEvent.click(screen.getByText('Cancel'));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('calls onClose when overlay is clicked', () => {
    const onClose = vi.fn();
    render(
      <SettingsPanel open={true} settings={defaultSettings} onSave={vi.fn()} onClose={onClose} />,
    );

    // Click the overlay outside the modal
    const overlay = document.getElementById('settings-overlay') as HTMLElement;
    fireEvent.click(overlay);
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('calls onClose on Escape key', () => {
    const onClose = vi.fn();
    render(
      <SettingsPanel open={true} settings={defaultSettings} onSave={vi.fn()} onClose={onClose} />,
    );

    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('resets to defaults when Reset is clicked', () => {
    const customSettings: AgentSettings = {
      model: 'custom-model',
      temperature: 1.5,
      maxIterations: 50,
      systemInstructions: 'Custom instructions.',
    };
    render(
      <SettingsPanel open={true} settings={customSettings} onSave={vi.fn()} onClose={vi.fn()} />,
    );

    fireEvent.click(screen.getByText('Reset to Defaults'));

    const modelInput = screen.getByPlaceholderText('e.g. gpt-4o, qwen3.6-27b');
    expect(modelInput).toHaveValue('');
  });
});

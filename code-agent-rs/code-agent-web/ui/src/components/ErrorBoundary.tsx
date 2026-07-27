import { Component, type ErrorInfo, type ReactNode } from 'react';

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

export default class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error('[ErrorBoundary]', error, errorInfo.componentStack);
  }

  handleReset = () => {
    this.setState({ hasError: false, error: null });
  };

  render() {
    if (this.state.hasError) {
      return (
        <div style={{
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
          height: '100vh',
          padding: '24px',
          color: 'var(--text-primary)',
          background: 'var(--bg-root)',
          fontFamily: 'var(--font-sans)',
          gap: '16px',
          textAlign: 'center',
        }}>
          <div style={{ fontSize: '32px', opacity: 0.3 }}>⚠</div>
          <h2 style={{ fontSize: '16px', fontWeight: 600, margin: 0 }}>Something went wrong</h2>
          <p style={{ fontSize: '12px', color: 'var(--text-secondary)', maxWidth: '400px', lineHeight: 1.6 }}>
            {this.state.error?.message || 'An unexpected error occurred.'}
          </p>
          <div style={{ fontSize: '11px', color: 'var(--text-muted)', maxWidth: '500px', fontFamily: 'var(--font-mono)', wordBreak: 'break-all' }}>
            {this.state.error?.stack?.split('\n').slice(0, 3).join('\n')}
          </div>
          <button
            onClick={this.handleReset}
            style={{
              marginTop: '8px',
              padding: '8px 24px',
              background: 'var(--accent)',
              color: '#fff',
              border: 'none',
              borderRadius: 'var(--radius-sm)',
              cursor: 'pointer',
              fontSize: '13px',
              fontWeight: 500,
            }}
          >
            Try Again
          </button>
        </div>
      );
    }

    return this.props.children;
  }
}

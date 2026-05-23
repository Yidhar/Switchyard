import React, { useEffect } from 'react';
import { X } from 'lucide-react';
import { TopologyGraph } from './ui/TopologyGraph';

interface TopologyOverlayProps {
  open: boolean;
  onClose: () => void;
  activeCore: string;
  enabledPeers: string[];
  activeNode: string | null;
  isGenerating: boolean;
  onNodeSelect?: (node: string) => void;
}

/// Fullscreen modal that hosts the TopologyGraph in a dedicated
/// surface, instead of cramming it into a sidebar tab. Triggered from
/// the diagnostics drawer header. Closes on Esc or backdrop click.
export const TopologyOverlay: React.FC<TopologyOverlayProps> = ({
  open,
  onClose,
  activeCore,
  enabledPeers,
  activeNode,
  isGenerating,
  onNodeSelect,
}) => {
  // Esc to dismiss. Bound while open so closed-state doesn't intercept
  // shortcuts the rest of the app might want.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      onClick={(e) => {
        // Backdrop clicks (outside the panel) dismiss; clicks inside
        // the panel itself bubble normally so node selection still
        // works.
        if (e.target === e.currentTarget) onClose();
      }}
      style={{
        position: 'fixed',
        inset: 0,
        background: 'rgba(0, 0, 0, 0.75)',
        backdropFilter: 'blur(6px)',
        zIndex: 50,
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        padding: 32,
      }}
    >
      <div
        style={{
          background: 'rgba(15, 17, 22, 0.98)',
          border: '1px solid var(--border-muted)',
          borderRadius: 8,
          boxShadow: '0 24px 48px rgba(0, 0, 0, 0.6)',
          width: '100%',
          maxWidth: 1200,
          height: '100%',
          maxHeight: 800,
          display: 'flex',
          flexDirection: 'column',
          overflow: 'hidden',
        }}
      >
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 12,
            padding: '12px 18px',
            borderBottom: '1px solid var(--border-muted)',
            background: 'rgba(255, 255, 255, 0.02)',
            flexShrink: 0,
          }}
        >
          <span
            style={{
              fontSize: 11,
              fontWeight: 700,
              color: 'var(--text-muted)',
              letterSpacing: '0.5px',
              textTransform: 'uppercase',
            }}
          >
            Topology
          </span>
          <span style={{ color: 'var(--text-secondary)', fontSize: 13 }}>
            Core <strong style={{ color: 'var(--text-primary)' }}>{activeCore}</strong>{' '}
            · Peers{' '}
            <strong style={{ color: 'var(--text-primary)' }}>
              {enabledPeers.length}
            </strong>
            {isGenerating && (
              <span
                style={{
                  marginLeft: 12,
                  color: 'var(--color-primary)',
                  fontSize: 12,
                }}
              >
                ● running
              </span>
            )}
          </span>
          <div style={{ flex: 1 }} />
          <span
            style={{ fontSize: 11, color: 'var(--text-muted)' }}
          >
            Press Esc to close
          </span>
          <button
            type="button"
            onClick={onClose}
            title="Close"
            style={{
              background: 'transparent',
              border: '1px solid var(--border-muted)',
              borderRadius: 4,
              color: 'var(--text-secondary)',
              cursor: 'pointer',
              padding: '4px 8px',
              display: 'inline-flex',
              alignItems: 'center',
              gap: 4,
            }}
          >
            <X size={14} />
          </button>
        </div>
        <div
          style={{
            flex: 1,
            minHeight: 0,
            overflow: 'auto',
            padding: 16,
          }}
        >
          <TopologyGraph
            activeCore={activeCore}
            enabledPeers={enabledPeers}
            activeNode={activeNode}
            isGenerating={isGenerating}
            onNodeSelect={onNodeSelect}
          />
        </div>
      </div>
    </div>
  );
};

export default TopologyOverlay;

import React from 'react';

interface TopologyGraphProps {
  activeCore: string;
  enabledPeers: string[];
  activeNode: string | null; // e.g., 'host', 'core', or the peer name like 'claude', 'gemini'
  isGenerating: boolean;
  onNodeSelect?: (nodeName: string) => void;
}

export const TopologyGraph: React.FC<TopologyGraphProps> = ({
  activeCore = 'codex',
  enabledPeers = [],
  activeNode = null,
  isGenerating = false,
  onNodeSelect,
}) => {
  // Ensure we have a set of peers to display. If none are enabled, display a default greyed-out peer for preview
  const displayPeers = enabledPeers.length > 0 ? enabledPeers : ['claude', 'gemini'];
  const hasNoPeersEnabled = enabledPeers.length === 0;

  const totalPeers = displayPeers.length;
  const getPeerX = (index: number, total: number) => {
    if (total <= 1) return 160;
    const padding = 60;
    const width = 320;
    const step = (width - padding * 2) / (total - 1);
    return padding + index * step;
  };

  const getStatusColor = (nodeName: string, isDefaultInactive = false) => {
    if (isDefaultInactive) return '#4b5563'; // gray
    
    // Check if this node is currently active
    const isCurrentActive = activeNode?.toLowerCase() === nodeName.toLowerCase();
    
    if (isCurrentActive) {
      if (nodeName.toLowerCase() === 'host') return '#64748b'; // slate
      if (nodeName.toLowerCase() === activeCore.toLowerCase()) return '#3b82f6'; // blue
      return '#f59e0b'; // orange for peers
    }
    
    // Default fallback colors
    if (nodeName.toLowerCase() === 'host') return '#94a3b8';
    if (nodeName.toLowerCase() === activeCore.toLowerCase()) return '#60a5fa';
    return '#9ca3af';
  };

  const isNodeActive = (nodeName: string) => {
    return activeNode?.toLowerCase() === nodeName.toLowerCase();
  };

  return (
    <div 
      className="topology-container"
      style={{
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        background: '#0f1015',
        borderRadius: '4px',
        padding: '16px',
        border: '1px solid var(--border-muted)',
      }}
    >
      <svg 
        viewBox="0 0 320 260" 
        width="100%" 
        height="220px" 
        style={{ background: 'transparent' }}
      >
        {/* Connection Edges */}
        
        {/* Link: Host -> Core */}
        <line 
          x1="160" y1="50" 
          x2="160" y2="120" 
          stroke={isNodeActive('host') && isGenerating ? '#64748b' : 'var(--border-muted)'} 
          strokeWidth="2" 
          strokeDasharray={isNodeActive('host') && isGenerating ? '4,4' : 'none'}
        />
        {isNodeActive('host') && isGenerating && (
          <circle cx="160" cy="50" r="4" fill="#64748b">
            <animate attributeName="cy" from="50" to="120" dur="1.2s" repeatCount="indefinite" />
          </circle>
        )}

        {/* Links: Core -> Peers */}
        {displayPeers.map((peer, idx) => {
          const peerX = getPeerX(idx, totalPeers);
          const isPeerActive = isNodeActive(peer);
          const edgeColor = isPeerActive && isGenerating 
            ? '#f59e0b' 
            : isNodeActive(activeCore) && isGenerating 
              ? '#3b82f6' 
              : 'var(--border-muted)';
          
          return (
            <g key={`edge-${peer}`}>
              <line 
                x1="160" y1="120" 
                x2={peerX} y2="200" 
                stroke={edgeColor} 
                strokeWidth={isPeerActive ? '2' : '1.5'} 
                strokeDasharray={isPeerActive && isGenerating ? '3,3' : 'none'}
                opacity={hasNoPeersEnabled ? 0.3 : 1}
              />
              {/* Flow indicator towards active peer */}
              {isPeerActive && isGenerating && (
                <circle cx="160" cy="120" r="3.5" fill="#f59e0b">
                  <animate attributeName="cx" from="160" to={peerX} dur="1.5s" repeatCount="indefinite" />
                  <animate attributeName="cy" from="120" to="200" dur="1.5s" repeatCount="indefinite" />
                </circle>
              )}
            </g>
          );
        })}

        {/* Nodes */}

        {/* 1. Host Node */}
        <g style={{ cursor: 'pointer' }} onClick={() => onNodeSelect?.('host')}>
          {isNodeActive('host') && (
            <circle cx="160" cy="50" r="24" fill="rgba(100, 116, 139, 0.1)" stroke="#64748b" strokeWidth="1" opacity="0.6">
              <animate attributeName="r" from="18" to="26" dur="2s" repeatCount="indefinite" />
              <animate attributeName="opacity" from="0.6" to="0" dur="2s" repeatCount="indefinite" />
            </circle>
          )}
          <circle 
            cx="160" cy="50" r="18" 
            fill="rgba(100, 116, 139, 0.15)" 
            stroke={getStatusColor('host')} 
            strokeWidth="2" 
          />
          <text 
            x="160" y="54" 
            textAnchor="middle" 
            fill="#fff" 
            fontSize="9" 
            fontWeight="bold"
            fontFamily="monospace"
          >
            HOST
          </text>
        </g>

        {/* 2. Core (Orchestrator) Node */}
        <g style={{ cursor: 'pointer' }} onClick={() => onNodeSelect?.(activeCore)}>
          {isNodeActive(activeCore) && isGenerating && (
            <circle cx="160" cy="120" r="28" fill="rgba(59, 130, 246, 0.1)" stroke="#3b82f6" strokeWidth="1" opacity="0.6">
              <animate attributeName="r" from="20" to="30" dur="2s" repeatCount="indefinite" />
              <animate attributeName="opacity" from="0.6" to="0" dur="2s" repeatCount="indefinite" />
            </circle>
          )}
          <circle 
            cx="160" cy="120" r="20" 
            fill="rgba(59, 130, 246, 0.15)" 
            stroke={getStatusColor(activeCore)} 
            strokeWidth="2.5" 
          />
          <text 
            x="160" y="124" 
            textAnchor="middle" 
            fill="#e5e7eb" 
            fontSize="9" 
            fontWeight="bold"
            style={{ textTransform: 'uppercase' }}
          >
            {activeCore.substring(0, 5)}
          </text>
        </g>

        {/* 3. Peer Nodes */}
        {displayPeers.map((peer, idx) => {
          const peerX = getPeerX(idx, totalPeers);
          const isPeerActive = isNodeActive(peer);
          
          return (
            <g key={`node-${peer}`} style={{ cursor: 'pointer' }} onClick={() => onNodeSelect?.(peer)}>
              {isPeerActive && isGenerating && (
                <circle cx={peerX} cy="200" r="24" fill="rgba(245, 158, 11, 0.1)" stroke="#f59e0b" strokeWidth="1" opacity="0.6">
                  <animate attributeName="r" from="16" to="26" dur="2s" repeatCount="indefinite" />
                  <animate attributeName="opacity" from="0.6" to="0" dur="2s" repeatCount="indefinite" />
                </circle>
              )}
              <circle 
                cx={peerX} cy="200" r="16" 
                fill={isPeerActive ? 'rgba(245, 158, 11, 0.15)' : 'rgba(255, 255, 255, 0.02)'} 
                stroke={getStatusColor(peer, hasNoPeersEnabled)} 
                strokeWidth="1.5" 
              />
              <text 
                x={peerX} y="203" 
                textAnchor="middle" 
                fill={hasNoPeersEnabled ? '#4b5563' : isPeerActive ? '#fde047' : '#9ca3af'} 
                fontSize="8" 
                fontWeight="bold"
                style={{ textTransform: 'uppercase' }}
              >
                {peer.substring(0, 4)}
              </text>
            </g>
          );
        })}
      </svg>
      <div 
        style={{ 
          fontSize: '11px', 
          color: 'var(--text-secondary)', 
          textAlign: 'center', 
          width: '100%', 
          marginTop: '6px',
          borderTop: '1px solid rgba(255,255,255,0.05)',
          paddingTop: '8px'
        }}
      >
        {isGenerating ? (
          <div>
            <span>Active: </span>
            <strong style={{ color: activeNode ? getStatusColor(activeNode) : 'var(--color-primary)', textTransform: 'uppercase' }}>
              {activeNode || 'Orchestrating...'}
            </strong>
          </div>
        ) : (
          <span style={{ color: 'var(--text-muted)' }}>Idle | Standby</span>
        )}
      </div>
    </div>
  );
};

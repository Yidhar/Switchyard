import React from 'react';
import { FolderOpen, Layers, Sparkles } from 'lucide-react';
import type { Workspace } from '../types';

interface WelcomeWorkspaceProps {
  workspaces: Workspace[];
  onOpenFolder: () => void;
  onSwitchWorkspace: (workspaceId: string) => void;
}

export const WelcomeWorkspace: React.FC<WelcomeWorkspaceProps> = ({
  workspaces,
  onOpenFolder,
  onSwitchWorkspace,
}) => {
  return (
    <div className="welcome-workspace">
      <div className="welcome-workspace-card">
        <div className="welcome-workspace-logo">
          <Sparkles size={34} />
        </div>
        <div>
          <div className="welcome-workspace-kicker">Switchyard Workbench</div>
          <h1>Open a folder to start</h1>
          <p>
            Pick a project folder to enable chat sessions, Explorer, Source
            Control, Canvas file editing, and the integrated terminal in the
            same workspace scope.
          </p>
        </div>

        <div className="welcome-workspace-actions">
          <button
            type="button"
            className="welcome-workspace-primary"
            onClick={onOpenFolder}
          >
            <FolderOpen size={16} />
            Open Folder…
          </button>
          <div className="welcome-workspace-tip">
            <Layers size={14} />
            Use Workspace → Add Folder to Workspace for multi-root projects.
          </div>
        </div>

        {workspaces.length > 0 && (
          <div className="welcome-workspace-recents">
            <div className="welcome-workspace-section-title">
              Recent Workspaces
            </div>
            {workspaces.slice(0, 8).map((workspace) => (
              <button
                key={workspace.workspace_id}
                type="button"
                className="welcome-workspace-recent"
                onClick={() => onSwitchWorkspace(workspace.workspace_id)}
                title={workspace.primary_root}
              >
                <FolderOpen size={14} />
                <span className="welcome-workspace-recent-name">
                  {workspace.name}
                </span>
                <span className="welcome-workspace-recent-path">
                  {workspace.primary_root}
                </span>
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
};

export default WelcomeWorkspace;

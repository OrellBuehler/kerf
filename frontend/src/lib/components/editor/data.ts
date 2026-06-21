/* Presentation-only constants shared by the editor components: the task-status
   presentation map and the agent preset prompts. All project data (assets,
   timeline, transcript, waveforms, tasks, history) renders from the real
   backend via `editor` / `agent`. */

import type { TaskStatus } from '$lib/types';

export const STATUS_MAP: Record<TaskStatus, { tone: string; icon: string; label: string }> = {
	queued: { tone: 'neutral', icon: 'clock', label: 'Queued' },
	working: { tone: 'agent', icon: 'loader', label: 'Working' },
	ready: { tone: 'success', icon: 'git-pull-request-arrow', label: 'Ready to review' },
	done: { tone: 'neutral', icon: 'check', label: 'Applied' },
	failed: { tone: 'neutral', icon: 'history', label: 'Failed' }
};

export const PRESETS = ['Remove silences', 'Assemble rough cut', 'Find best 60s', 'Color match'];

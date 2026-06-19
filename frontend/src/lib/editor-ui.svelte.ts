/* Editor chrome + the agent workflow state machine.
   The cut workflow (empty → analyzing → review → editing) is driven here as
   a UI-level affordance: Kerf has no in-app AI, so until a connected agent
   claims tasks over MCP, the status-bar "Demo state" selector and the import
   action move through the same states the design showcases. */

export type EditorPhase = 'empty' | 'analyzing' | 'review' | 'editing';
export type Tool = 'pointer' | 'razor' | 'bookmark';

class EditorUi {
	phase = $state<EditorPhase>('empty');
	tool = $state<Tool>('pointer');
	snap = $state(true);
	agentOpen = $state(true);
	playing = $state(false);
	progress = $state(0);

	#timer: ReturnType<typeof setInterval> | null = null;
	#advance: ReturnType<typeof setTimeout> | null = null;

	#clear() {
		if (this.#timer) clearInterval(this.#timer);
		if (this.#advance) clearTimeout(this.#advance);
		this.#timer = null;
		this.#advance = null;
	}

	setPhase(phase: EditorPhase) {
		this.#clear();
		this.phase = phase;
		if (phase === 'analyzing') this.#runAnalysis();
	}

	#runAnalysis() {
		this.progress = 8;
		this.#timer = setInterval(() => {
			if (this.progress >= 100) {
				this.#clear();
				this.#advance = setTimeout(() => this.setPhase('review'), 500);
				return;
			}
			this.progress = Math.min(100, this.progress + 8);
		}, 220);
	}

	startAnalyze() {
		this.progress = 0;
		this.setPhase('analyzing');
	}

	apply() {
		this.setPhase('editing');
	}

	reject() {
		this.setPhase('editing');
	}
}

export const ui = new EditorUi();

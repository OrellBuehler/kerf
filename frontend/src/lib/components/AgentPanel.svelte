<script lang="ts">
	import * as Tabs from '$lib/components/ui/tabs';
	import { Button } from '$lib/components/ui/button';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Badge } from '$lib/components/ui/badge';
	import { ScrollArea } from '$lib/components/ui/scroll-area';
	import { Sparkles, Send, Wrench } from '@lucide/svelte';
	import { editor } from '$lib/state.svelte';

	type Message = { role: 'user' | 'assistant'; text: string };

	let input = $state('');
	let messages = $state<Message[]>([
		{
			role: 'assistant',
			text: 'Hi! I can analyze your media and assemble edits through the Kerf MCP server. Try asking me to cut a clip or remove silence. (This panel is a placeholder — wire it to your LLM of choice.)'
		}
	]);

	// Mirrors the tools exposed by the kerf-mcp stdio server.
	const tools: { name: string; desc: string }[] = [
		{ name: 'list_assets', desc: 'List all media assets' },
		{ name: 'get_asset_metadata', desc: 'Probed metadata + analysis' },
		{ name: 'get_timeline_state', desc: 'Full timeline / EDL' },
		{ name: 'cut_clip', desc: 'Append a cut of an asset' },
		{ name: 'add_clip_to_timeline', desc: 'Add a clip to a track' },
		{ name: 'split_at', desc: 'Split a clip at a time' },
		{ name: 'trim', desc: 'Trim a clip in/out' },
		{ name: 'reorder', desc: 'Reorder clips in a track' },
		{ name: 'remove', desc: 'Remove a clip' },
		{ name: 'set_volume', desc: 'Set a clip volume' },
		{ name: 'remove_silence', desc: 'Cut out silent spans' },
		{ name: 'extract_audio', desc: 'Append asset audio' },
		{ name: 'concatenate', desc: 'Stitch assets in order' },
		{ name: 'export', desc: 'Render the timeline' }
	];

	function send() {
		const text = input.trim();
		if (!text) return;
		messages = [
			...messages,
			{ role: 'user', text },
			{
				role: 'assistant',
				text: `Connect an LLM to act on: "${text}". It would call the Kerf MCP tools (e.g. cut_clip, remove_silence) to edit the timeline.`
			}
		];
		input = '';
	}
</script>

<div class="flex h-full flex-col">
	<div class="flex items-center gap-2 border-b px-3 py-2">
		<Sparkles class="text-primary size-4" />
		<h2 class="text-sm font-semibold">AI agent</h2>
	</div>

	<Tabs.Root value="chat" class="flex min-h-0 flex-1 flex-col">
		<Tabs.List class="mx-3 mt-2 grid grid-cols-3">
			<Tabs.Trigger value="chat">Chat</Tabs.Trigger>
			<Tabs.Trigger value="tools">Tools</Tabs.Trigger>
			<Tabs.Trigger value="inspect">Inspect</Tabs.Trigger>
		</Tabs.List>

		<Tabs.Content value="chat" class="flex min-h-0 flex-1 flex-col">
			<ScrollArea class="flex-1">
				<div class="flex flex-col gap-3 p-3">
					{#each messages as msg, i (i)}
						<div
							class="max-w-[90%] rounded-lg px-3 py-2 text-sm {msg.role === 'user'
								? 'bg-primary text-primary-foreground self-end'
								: 'bg-muted self-start'}"
						>
							{msg.text}
						</div>
					{/each}
				</div>
			</ScrollArea>
			<div class="flex gap-2 border-t p-3">
				<Textarea
					bind:value={input}
					placeholder="Ask the agent to edit…"
					class="max-h-28 min-h-9 resize-none"
					onkeydown={(e) => {
						if (e.key === 'Enter' && !e.shiftKey) {
							e.preventDefault();
							send();
						}
					}}
				/>
				<Button size="icon" onclick={send} aria-label="Send">
					<Send class="size-4" />
				</Button>
			</div>
		</Tabs.Content>

		<Tabs.Content value="tools" class="min-h-0 flex-1">
			<ScrollArea class="h-full">
				<div class="flex flex-col gap-1.5 p-3">
					{#each tools as tool (tool.name)}
						<div class="flex items-start gap-2 rounded-md border p-2">
							<Wrench class="text-muted-foreground mt-0.5 size-3.5 shrink-0" />
							<div class="min-w-0">
								<code class="text-xs font-medium">{tool.name}</code>
								<p class="text-muted-foreground text-xs">{tool.desc}</p>
							</div>
						</div>
					{/each}
				</div>
			</ScrollArea>
		</Tabs.Content>

		<Tabs.Content value="inspect" class="min-h-0 flex-1">
			<ScrollArea class="h-full">
				<div class="flex flex-col gap-3 p-3 text-sm">
					{#if editor.selectedMetadata}
						{@const m = editor.selectedMetadata}
						<div>
							<div class="mb-1 font-medium">{m.asset.name}</div>
							<div class="text-muted-foreground text-xs">{m.asset.duration.toFixed(1)}s</div>
						</div>
						{#if m.analysis}
							<div>
								<div class="mb-1 flex items-center gap-1.5 text-xs font-semibold">
									Scene changes <Badge variant="secondary">{m.analysis.scene_changes.length}</Badge>
								</div>
								<div class="text-muted-foreground text-xs">
									{m.analysis.scene_changes.map((t) => t.toFixed(0) + 's').join(', ') || '—'}
								</div>
							</div>
							<div>
								<div class="mb-1 flex items-center gap-1.5 text-xs font-semibold">
									Silence <Badge variant="secondary">{m.analysis.silence_segments.length}</Badge>
								</div>
								<div class="text-muted-foreground text-xs">
									{m.analysis.silence_segments
										.map((s) => `${s.start.toFixed(1)}–${s.end.toFixed(1)}s`)
										.join(', ') || '—'}
								</div>
							</div>
							<div>
								<div class="mb-1 text-xs font-semibold">Transcript</div>
								<div class="flex flex-col gap-1">
									{#each m.analysis.transcript as seg, i (i)}
										<div class="text-xs">
											<span class="text-muted-foreground font-mono">{seg.start.toFixed(1)}s</span>
											{seg.text}
										</div>
									{:else}
										<span class="text-muted-foreground text-xs">No transcript.</span>
									{/each}
								</div>
							</div>
						{:else}
							<p class="text-muted-foreground text-xs">No analysis cached for this asset.</p>
						{/if}
					{:else}
						<p class="text-muted-foreground text-xs">Select an asset to inspect its analysis.</p>
					{/if}
				</div>
			</ScrollArea>
		</Tabs.Content>
	</Tabs.Root>
</div>

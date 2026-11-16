//! The agent task queue.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddTaskParams {
    #[schemars(description = "What the task should accomplish, in plain language")]
    prompt: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CompleteTaskParams {
    #[schemars(description = "UUID of the task to complete")]
    task_id: String,
    #[schemars(description = "Short summary of the edits made, shown to the user")]
    result: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FailTaskParams {
    #[schemars(description = "UUID of the task that could not be completed")]
    task_id: String,
    #[schemars(description = "Why the task failed")]
    error: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TaskIdParams {
    #[schemars(description = "UUID of the task")]
    task_id: String,
}

#[tool_router(router = tool_router_tasks, vis = "pub(super)")]
impl KerfMcp {
    // ---- agent task queue --------------------------------------------------

    #[tool(
        description = "List the agent task queue with each task's status (queued/working/ready/done/failed), oldest first: \
                       `tasks` and the `total` count. Returns the newest `limit` tasks (default 50, at most 500) after \
                       skipping `offset` of the newest; `next_offset` pages back through older ones."
    )]
    fn list_tasks(&self, Parameters(p): Parameters<PageParams>) -> Result<String, McpError> {
        tasks_page(&self.lock(), p)
    }

    #[tool(description = "Enqueue a new task (status: queued) for an agent to claim")]
    fn add_task(&self, Parameters(p): Parameters<AddTaskParams>) -> Result<String, McpError> {
        self.act(|project| {
            let task = project.add_task(&p.prompt).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(
        description = "Claim the oldest queued task (marks it working) and return it; returns null when the queue is empty. \
                       With the user's review on (the default) claiming opens a proposal for the task: your edits go \
                       into it instead of the live cut, and the user reviews and accepts it in Kerf. With review off no \
                       proposal is opened: each edit lands on the live cut as you make it, an undoable revision \
                       attributed to you. Refused while another proposal is open (it must be applied or discarded first)."
    )]
    fn claim_next_task(&self) -> Result<String, McpError> {
        let owner = self.owner();
        self.act(|project| {
            let task = project.claim_next_task(owner.as_deref()).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(
        description = "Hand a claimed task over, with a summary of the edits made. With review on (the default) it is \
                       marked ready: the proposal waits for the user to accept it in Kerf. With review off and no proposal \
                       of yours open, your edits have already landed on the live cut, so it is marked done (a proposal \
                       opened with stage_edits still waiting leaves it ready)."
    )]
    fn complete_task(&self, Parameters(p): Parameters<CompleteTaskParams>) -> Result<String, McpError> {
        let id = parse_arg("task_id", &p.task_id)?;
        self.act(|project| {
            let task = project.complete_task(id, p.result).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(
        description = "Mark a task failed with an error message; a proposal staged under it is discarded. With review \
                       off the edits you already made stay on the live cut (undo steps them back), so say what is left."
    )]
    fn fail_task(&self, Parameters(p): Parameters<FailTaskParams>) -> Result<String, McpError> {
        let id = parse_arg("task_id", &p.task_id)?;
        self.act(|project| {
            let task = project.fail_task(id, &p.error).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(
        description = "Mark a task done, returning it. This is what the user's accepting the proposal does; if the task still has a staged proposal it is applied with it, which an agent is refused while review is on (the user applies agent edits in Kerf), so do not call it to finish a task — complete_task is how you hand work over."
    )]
    fn resolve_task(&self, Parameters(p): Parameters<TaskIdParams>) -> Result<String, McpError> {
        let id = parse_arg("task_id", &p.task_id)?;
        self.act(|project| {
            let task = project.resolve_task(id).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(
        description = "Remove a task from the queue permanently, and with it the proposal staged under it (dismissing a task \
                       is how the user says no to its edits; the proposal can still be brought back with \
                       restore_discarded_edits). Refused, with the task left in the queue, when another agent session owns \
                       that proposal. Returns the id removed and how many tasks remain (list_tasks has them)."
    )]
    fn remove_task(&self, Parameters(p): Parameters<TaskIdParams>) -> Result<String, McpError> {
        let id = parse_arg("task_id", &p.task_id)?;
        self.act(|project| {
            project.remove_task(id).map_err(core_err)?;
            let remaining = project.list_tasks_page(kerf_core::Page::default()).map_err(core_err)?.1;
            json(&serde_json::json!({ "removed": id, "remaining_tasks": remaining }))
        })
    }
}

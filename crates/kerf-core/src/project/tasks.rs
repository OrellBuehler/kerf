//! The agent task queue.

use super::*;

impl Project {
    // ---- agent task queue -------------------------------------------------

    /// Enqueue a task for a connected agent to claim. Returns the new `queued`
    /// task.
    pub fn add_task(&self, prompt: &str) -> Result<Task> {
        let now = Utc::now();
        let task = Task {
            id: Uuid::new_v4(),
            prompt: prompt.to_string(),
            status: TaskStatus::Queued,
            result: None,
            created_at: now,
            updated_at: now,
        };
        self.upsert_task(&task)?;
        Ok(task)
    }

    fn upsert_task(&self, task: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tasks (id, prompt, status, result, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                task.id.to_string(),
                task.prompt,
                task.status.as_str(),
                task.result,
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        Ok(self.list_tasks_page(Page::default())?.0)
    }

    /// A window onto the task queue, with how many tasks there are in all — `page.offset` of the
    /// newest skipped, up to `page.limit` of the ones before them returned, oldest first (queue
    /// order), the same window as [`Project::history_page`].
    pub fn list_tasks_page(&self, page: Page) -> Result<(Vec<Task>, usize)> {
        let total: i64 = self.conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?;
        let mut stmt = self.conn.prepare(
            "SELECT id, prompt, status, result, created_at, updated_at FROM
                 (SELECT id, prompt, status, result, created_at, updated_at FROM tasks
                  ORDER BY created_at DESC LIMIT ?1 OFFSET ?2)
             ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![page.sql_limit(), page.offset as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            let (id, prompt, status, result, created_at, updated_at) = row?;
            tasks.push(row_to_task(id, prompt, status, result, created_at, updated_at)?);
        }
        Ok((tasks, total as usize))
    }

    pub fn get_task(&self, id: Uuid) -> Result<Option<Task>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, prompt, status, result, created_at, updated_at FROM tasks WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        match row {
            Some((id, prompt, status, result, created_at, updated_at)) => {
                Ok(Some(row_to_task(id, prompt, status, result, created_at, updated_at)?))
            }
            None => Ok(None),
        }
    }

    pub fn require_task(&self, id: Uuid) -> Result<Task> {
        self.get_task(id)?.ok_or(Error::TaskNotFound(id))
    }

    /// Mark a specific task `working` (an agent has claimed it).
    pub fn claim_task(&self, id: Uuid) -> Result<Task> {
        self.set_task_state(id, TaskStatus::Working, None)
    }

    /// Claim the oldest `queued` task, marking it `working`. Returns `None` when
    /// nothing is waiting — the agent's "give me work" primitive.
    ///
    /// With review on (the default) claiming opens a staging session for the task, so the work an
    /// agent does on the user's behalf is a proposal and never rewrites the open cut unasked;
    /// `owner` is the agent session (MCP session id) that proposal will belong to. With review
    /// off **no proposal is opened**: the task is only marked `working`, and the agent's edits land
    /// on the live cut one by one, as ordinary revisions attributed to the agent.
    pub fn claim_next_task(&self, owner: Option<&str>) -> Result<Option<Task>> {
        let next: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM tasks WHERE status = 'queued' ORDER BY created_at LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = next else { return Ok(None) };
        let id = parse_uuid(&id)?;
        // A session already open for a *different* task (or for none) must not silently absorb
        // this one's edits too — that folds two tasks' work into a single proposal and leaves the
        // second task with no review of its own (and, with review off, would hold back work that
        // was meant to land) — so refuse before touching this task's state at all. That goes for a
        // proposal that was open when the user turned review off as much as for one opened on
        // purpose. The one exception is a session whose owning task has already gone terminal:
        // `fail_task` used to leave one behind with nothing to ever clean it up, which wedged the
        // queue for good, so a claim is also the chance to clear it out.
        let review = self.agent_review.get();
        match self.staged_row()? {
            Some(row) if row.task_id == Some(id) => {}
            Some(row) => {
                let terminal = match row.task_id {
                    Some(owner) => self
                        .get_task(owner)?
                        .map(|t| matches!(t.status, TaskStatus::Done | TaskStatus::Failed))
                        .unwrap_or(true),
                    None => false,
                };
                if !terminal {
                    return Err(Error::StagedEditPending);
                }
                // Nobody's work any more (its task is over), so whoever claims next may clear it.
                self.discard_row(&row)?;
                if review {
                    self.begin_staging(Some(id), None, owner)?;
                }
            }
            None => {
                if review {
                    self.begin_staging(Some(id), None, owner)?;
                }
            }
        }
        Ok(Some(self.set_task_state(id, TaskStatus::Working, None)?))
    }

    /// Hand a claimed task over, recording the agent's summary. The task is `ready` when its
    /// work waits for the user — review is on, where accepting is theirs, or a proposal is open
    /// that is the task's or that the calling agent holds — and `done` when review is off and
    /// nothing is held back: the edits already landed on the live cut as they were made, so
    /// there is nothing to review and the task is only settled.
    pub fn complete_task(&self, id: Uuid, result: Option<String>) -> Result<Task> {
        let status = if self.task_work_waits(id)? {
            TaskStatus::Ready
        } else {
            TaskStatus::Done
        };
        self.set_task_state(id, status, Some(result))
    }

    /// Whether the work of task `id` is held back for the user: always with review on, and with it
    /// off while a proposal is open that is the task's, that the calling agent holds, or that
    /// nobody does. A proposal another agent session owns is that session's, not this task's.
    fn task_work_waits(&self, id: Uuid) -> Result<bool> {
        if self.agent_review.get() {
            return Ok(true);
        }
        Ok(match self.staged_row()? {
            None => false,
            Some(row) => row.task_id == Some(id) || !owned_by_other(row.owner.as_deref(), self.agent_owner.borrow().as_deref()),
        })
    }

    /// Mark a task `failed`, recording the error, and drop any proposal staged
    /// under it — like `remove_task`, a task that will never be resolved must
    /// not leave its session behind to wedge the queue. With review off and no proposal there is
    /// nothing to drop: the task is only settled, and what the agent already did to the live cut
    /// stays in the history (undoable like any revision) rather than being rolled back.
    pub fn fail_task(&self, id: Uuid, error: &str) -> Result<Task> {
        if self.staged_row()?.is_some_and(|r| r.task_id == Some(id)) {
            self.discard_staged()?;
        }
        self.set_task_state(id, TaskStatus::Failed, Some(Some(error.to_string())))
    }

    /// Mark a task `done` — the user accepted its work, so a proposal staged
    /// under it is applied in the same breath.
    pub fn resolve_task(&self, id: Uuid) -> Result<Task> {
        if self.staged_row()?.is_some_and(|r| r.task_id == Some(id)) {
            self.apply_staged(false)?;
        }
        self.set_task_state(id, TaskStatus::Done, None)
    }

    /// Drop a task, and with it any proposal staged under it — dismissing the
    /// task is how the user says no to the edit.
    ///
    /// The proposal goes first and the whole is one transaction: an agent that does not own the
    /// proposal ([`Error::StagedEditOwnedByOther`]) is refused with the task still in the queue,
    /// not after it was deleted.
    pub fn remove_task(&self, id: Uuid) -> Result<()> {
        self.require_task(id)?;
        let tx = self.tx()?;
        if self.staged_row()?.is_some_and(|r| r.task_id == Some(id)) {
            self.discard_staged()?;
        }
        self.conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id.to_string()])?;
        tx.commit()?;
        Ok(())
    }

    /// Transition a task. `result == None` leaves the stored result untouched;
    /// `Some(value)` overwrites it (with `value` itself possibly `None`).
    pub(super) fn set_task_state(&self, id: Uuid, status: TaskStatus, result: Option<Option<String>>) -> Result<Task> {
        let mut task = self.require_task(id)?;
        task.status = status;
        if let Some(value) = result {
            task.result = value;
        }
        task.updated_at = Utc::now();
        self.upsert_task(&task)?;
        Ok(task)
    }
}

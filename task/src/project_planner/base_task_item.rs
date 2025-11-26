use windows::Win32::Foundation::{COLORREF, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{Rectangle, SetDCBrushColor, SetDCPenColor, HDC};

use data::{model_utils::UNITS_PER_HOUR, service::Service, task::Task, types::Id, value::Value};

use crate::project_planner::base_tree_item::BaseTreeItem;
use crate::project_planner::project_plan_task_attributes::PROJECT_PLAN_SHOW_CHILDREN;

pub struct BaseTaskItem {
    pub base: BaseTreeItem,
    pub task_id: Id,
    service: *mut Service,
}

impl BaseTaskItem {
    pub fn new(task_id: Id, service: *mut Service) -> Self {
        let mut base = BaseTreeItem::new();
        if let Some(expanded) = Self::expanded_property(service, task_id) {
            base.expanded = expanded;
        }
        Self {
            base,
            task_id,
            service,
        }
    }

    pub fn task(&self) -> Option<&Task> {
        unsafe { self.service.as_ref() }?.tasks.get(&self.task_id)
    }

    pub fn task_name(&self) -> String {
        self.task()
            .map(|task| task.name.clone())
            .unwrap_or_else(|| "Task".to_string())
    }

    pub fn estimated_duration(&self) -> u64 {
        let est = self.task().map(|task| task.estimated_duration).unwrap_or(0);
        if est == 0 {
            UNITS_PER_HOUR
        } else {
            est
        }
    }

    pub fn actual_time(&self) -> u64 {
        let service = match unsafe { self.service.as_ref() } {
            Some(service) => service,
            None => return 0,
        };
        service
            .time_entries
            .values()
            .filter(|entry| entry.task_id == self.task_id)
            .map(|entry| entry.duration)
            .sum()
    }

    pub fn performance_percent(&self) -> f64 {
        let est = self.estimated_duration() as f64;
        if est <= 0.0 {
            return 0.0;
        }
        let actual = self.actual_time() as f64;
        (actual / est).min(1.0)
    }

    pub fn user_percent(&self) -> f64 {
        if let Some(task) = self.task() {
            if let Some(Value::U64(val)) = task.properties.get("percent_complete") {
                return (*val as f64 / 100.0).clamp(0.0, 1.0);
            }
        }
        0.0
    }

    pub fn target_percent(&self) -> f64 {
        if let Some(task) = self.task() {
            let start = task.scheduled_start;
            let due = task.due_date;
            if due > start && start > 0 {
                let now = data::model_utils::now_timestamp();
                let elapsed = now.saturating_sub(start) as f64;
                let total = (due - start) as f64;
                return (elapsed / total).clamp(0.0, 1.0);
            }
        }
        0.0
    }

    pub fn background_color(&self) -> COLORREF {
        let user = self.user_percent();
        let target = self.target_percent();
        if user >= target {
            COLORREF(0x0078c887)
        } else if (target - user) >= 0.6 {
            COLORREF(0x00f39682)
        } else {
            COLORREF(0x00ebeb64)
        }
    }

    pub fn paint_status_graph(&self, dc: HDC, rect: RECT) {
        unsafe {
            let width = (rect.right - rect.left).max(0);
            SetDCPenColor(dc, COLORREF(0x00ffffff));
            SetDCBrushColor(dc, COLORREF(0x00ffffff));
            Rectangle(dc, rect.left, rect.top, rect.right, rect.bottom);

            let user_len = (width as f64 * self.user_percent()) as i32;
            let perf_len = (width as f64 * self.performance_percent()) as i32;
            let targ_len = (width as f64 * self.target_percent()) as i32;

            SetDCPenColor(dc, COLORREF(0x004119d7));
            SetDCBrushColor(dc, COLORREF(0x004119d7));
            Rectangle(dc, rect.left, rect.top, rect.left + user_len, rect.top + 4);

            SetDCPenColor(dc, COLORREF(0x0041b9c8));
            SetDCBrushColor(dc, COLORREF(0x0041b9c8));
            Rectangle(
                dc,
                rect.left,
                rect.top + 4,
                rect.left + perf_len,
                rect.top + 8,
            );

            SetDCPenColor(dc, COLORREF(0x00b9d741));
            SetDCBrushColor(dc, COLORREF(0x00b9d741));
            Rectangle(
                dc,
                rect.left,
                rect.top + 8,
                rect.left + targ_len,
                rect.top + 12,
            );
        }
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        self.base.expanded = expanded;
        if let Some(service) = unsafe { self.service.as_mut() } {
            service.update_task_property(
                self.task_id,
                PROJECT_PLAN_SHOW_CHILDREN.to_string(),
                Value::Bool(expanded),
            );
        }
    }

    fn expanded_property(service: *mut Service, task_id: Id) -> Option<bool> {
        let task = unsafe { service.as_ref() }?.tasks.get(&task_id)?;
        match task.properties.get(PROJECT_PLAN_SHOW_CHILDREN) {
            Some(Value::Bool(val)) => Some(*val),
            _ => None,
        }
    }
}

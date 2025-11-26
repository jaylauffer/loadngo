use data::{service::Service, task::Task, types::Id};

use crate::project_planner::base_tree_item::{
    hit_test_tree, set_selected, toggle_expanded_by_id, TreeItem,
};
use crate::project_planner::milestone_tree_item::MilestoneTreeItem;
use crate::project_planner::project_tree_item::ProjectTreeItem;
use crate::project_planner::task_tree_item::TaskTreeItem;

pub struct ProjectTreeAdapter {
    service: *mut Service,
    root_id: Option<Id>,
    root: Option<Box<dyn TreeItem>>,
    tier_count: i32,
}

impl ProjectTreeAdapter {
    pub fn new(service: *mut Service) -> Self {
        Self {
            service,
            root_id: None,
            root: None,
            tier_count: 0,
        }
    }

    pub fn set_root(&mut self, task_id: Option<Id>) {
        let root_id = task_id.or_else(|| self.default_root_id());
        self.root_id = root_id;
        self.root = None;
        if let Some(id) = root_id {
            if let Some(root_task) = self.get_task(id).cloned() {
                let mut root_item =
                    Box::new(ProjectTreeItem::new(root_task.entity.id, self.service));
                let children = self.child_tasks(root_task.entity.id);
                for child in children {
                    let milestone = self.build_milestone(&child);
                    root_item.base_mut().children.push(milestone);
                }
                self.tier_count = Self::depth(root_item.as_ref(), 0);
                self.root = Some(root_item);
            }
        }
    }

    pub fn root(&self) -> Option<&Box<dyn TreeItem>> {
        self.root.as_ref()
    }

    pub fn root_mut(&mut self) -> Option<&mut Box<dyn TreeItem>> {
        self.root.as_mut()
    }

    pub fn tier_count(&self) -> i32 {
        self.tier_count
    }

    pub fn to_report_xml(&self) -> String {
        let mut xml = String::from("<project_report>");
        if let Some(root) = self.root.as_ref() {
            xml.push_str(&root.to_report_xml());
        }
        xml.push_str("</project_report>");
        xml
    }

    pub fn update_selection(&mut self, selected: Option<Id>) {
        if let Some(root) = self.root.as_mut() {
            set_selected(root.as_mut(), selected);
        }
    }

    pub fn toggle_expanded(&mut self, id: Id) -> bool {
        if let Some(root) = self.root.as_mut() {
            return toggle_expanded_by_id(root.as_mut(), id);
        }
        false
    }

    pub fn hit_test(&self, pt: windows::Win32::Foundation::POINT) -> Option<(Id, bool)> {
        self.root
            .as_ref()
            .and_then(|root| hit_test_tree(root.as_ref(), pt))
    }

    fn build_milestone(&self, task: &Task) -> Box<dyn TreeItem> {
        let mut milestone = Box::new(MilestoneTreeItem::new(task.entity.id, self.service));
        self.add_task_children(milestone.as_mut(), task.entity.id);
        milestone
    }

    fn add_task_children(&self, parent: &mut dyn TreeItem, parent_id: Id) {
        let children = self.child_tasks(parent_id);
        for child in children {
            let mut item = Box::new(TaskTreeItem::new(child.entity.id, self.service));
            self.add_task_children(item.as_mut(), child.entity.id);
            parent.base_mut().children.push(item);
        }
    }

    fn child_tasks(&self, parent_id: Id) -> Vec<Task> {
        let service = match unsafe { self.service.as_ref() } {
            Some(service) => service,
            None => return Vec::new(),
        };
        let mut tasks: Vec<Task> = service
            .tasks
            .values()
            .filter(|task| task.parent == Some(parent_id))
            .cloned()
            .collect();
        tasks.sort_by(|a, b| a.name.cmp(&b.name));
        tasks
    }

    fn default_root_id(&self) -> Option<Id> {
        let service = unsafe { self.service.as_ref() }?;
        let mut roots: Vec<&Task> = service
            .tasks
            .values()
            .filter(|t| t.parent.is_none())
            .collect();
        roots.sort_by(|a, b| a.name.cmp(&b.name));
        roots.first().map(|task| task.entity.id)
    }

    fn get_task(&self, id: Id) -> Option<&Task> {
        unsafe { self.service.as_ref() }?.tasks.get(&id)
    }

    fn depth(item: &dyn TreeItem, depth: i32) -> i32 {
        let mut max_depth = depth.max(0);
        for child in item.base().children.iter() {
            max_depth = max_depth.max(Self::depth(child.as_ref(), depth + 1));
        }
        max_depth
    }
}

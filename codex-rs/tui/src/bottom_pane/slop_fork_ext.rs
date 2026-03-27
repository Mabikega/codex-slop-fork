use super::BottomPane;

impl BottomPane {
    pub(crate) fn dismiss_view_if_active(&mut self, view_id: &'static str) -> bool {
        let is_match = self
            .view_stack
            .last()
            .is_some_and(|view| view.view_id() == Some(view_id));
        if !is_match {
            return false;
        }

        self.view_stack.pop();
        self.on_active_view_complete();
        self.request_redraw();
        true
    }

    pub(crate) fn is_view_active(&self, view_id: &'static str) -> bool {
        self.view_stack
            .last()
            .is_some_and(|view| view.view_id() == Some(view_id))
    }
}

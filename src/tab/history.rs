use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Tab 历史栈（默认容量 20，用于恢复已关闭的标签页）
#[derive(Debug, Clone)]
pub struct TabHistoryStack {
    inner: Arc<Mutex<VecDeque<String>>>,
    max_capacity: usize,
}

impl TabHistoryStack {
    pub fn new(max_capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(max_capacity))),
            max_capacity,
        }
    }

    /// 记录一次被关闭的文件夹绝对路径（自动去重相邻重复项）
    pub fn push(&self, path: String) {
        if path.is_empty() {
            return;
        }

        if let Ok(mut lock) = self.inner.lock() {
            // 如果栈顶已经是该路径，不重复推入
            if lock.back().map(|p| p.as_str()) == Some(path.as_str()) {
                return;
            }

            if lock.len() >= self.max_capacity {
                lock.pop_front();
            }
            lock.push_back(path);
        }
    }

    /// 弹出最近一次关闭的路径以供恢复
    pub fn pop(&self) -> Option<String> {
        if let Ok(mut lock) = self.inner.lock() {
            lock.pop_back()
        } else {
            None
        }
    }

    /// 查看最近一次关闭的路径（不弹出）
    pub fn peek(&self) -> Option<String> {
        if let Ok(lock) = self.inner.lock() {
            lock.back().cloned()
        } else {
            None
        }
    }

    /// 获取历史栈中记录条数
    pub fn len(&self) -> usize {
        self.inner.lock().map(|l| l.len()).unwrap_or(0)
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 清空历史记录
    pub fn clear(&self) {
        if let Ok(mut lock) = self.inner.lock() {
            lock.clear();
        }
    }
}

impl Default for TabHistoryStack {
    fn default() -> Self {
        Self::new(20)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_history_push_pop() {
        let stack = TabHistoryStack::new(3);
        stack.push("C:\\Folder1".to_string());
        stack.push("C:\\Folder2".to_string());
        // 重复推入相邻项
        stack.push("C:\\Folder2".to_string());
        assert_eq!(stack.len(), 2);

        stack.push("C:\\Folder3".to_string());
        stack.push("C:\\Folder4".to_string()); // 超过容量 3，Folder1 被移出
        assert_eq!(stack.len(), 3);

        assert_eq!(stack.pop(), Some("C:\\Folder4".to_string()));
        assert_eq!(stack.pop(), Some("C:\\Folder3".to_string()));
        assert_eq!(stack.pop(), Some("C:\\Folder2".to_string()));
        assert_eq!(stack.pop(), None);
    }
}

//! 环形缓冲区（固定容量，自动淘汰最旧元素）
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §4.4 日志环形缓冲`
//!
//! 设计要点：
//! - 基于 `VecDeque` 实现，O(1) push / O(n) tail
//! - 容量固定：超出后自动 `pop_front` 最旧元素
//! - 仅在 `LogPipeline` 后台 flush task 中持写锁，写频率 ≤ 10次/s
//! - `tail(n)` 返回最近 n 条（倒序），用于 `process_tail_log` 命令

use std::collections::VecDeque;

/// 固定容量的环形缓冲区
///
/// 泛型 `T` 通常为 `String`（日志行），但也可用于其他场景。
pub struct RingBuffer<T> {
    buffer: VecDeque<T>,
    capacity: usize,
}

impl<T> RingBuffer<T> {
    /// 创建指定容量的环形缓冲区
    ///
    /// `capacity` 为 0 时退化为永远为空（不做任何保留）
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// 容量
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 当前元素数
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// 追加元素；若已满则淘汰最旧元素
    pub fn push(&mut self, item: T) {
        if self.capacity == 0 {
            return;
        }
        if self.buffer.len() == self.capacity {
            self.buffer.pop_front();
        }
        self.buffer.push_back(item);
    }

    /// 批量追加（按顺序 push，超容量自动淘汰）
    pub fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }

    /// 取最近 n 条（倒序，最新的在前）
    ///
    /// 若 n 大于当前元素数，返回全部（倒序）。
    /// 返回 `&T` 引用，调用方按需 clone。
    pub fn tail(&self, n: usize) -> Vec<&T> {
        self.buffer.iter().rev().take(n).collect()
    }

    /// 取最近 n 条并 clone（顺序与 `tail` 一致：最新的在前）
    pub fn tail_cloned(&self, n: usize) -> Vec<T>
    where
        T: Clone,
    {
        self.tail(n).into_iter().cloned().collect()
    }

    /// 清空缓冲区
    pub fn clear(&mut self) {
        self.buffer.clear();
    }
}

impl<T> Default for RingBuffer<T> {
    fn default() -> Self {
        Self::new(5000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_within_capacity() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.tail(10), vec![&3, &2, &1]);
    }

    #[test]
    fn test_push_overflow_evicts_oldest() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);
        buf.push(4); // 容量满，淘汰 1
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.tail(10), vec![&4, &3, &2]);
    }

    #[test]
    fn test_tail_n_larger_than_len() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(10);
        buf.push(1);
        buf.push(2);
        let tail = buf.tail(100);
        assert_eq!(tail, vec![&2, &1]);
    }

    #[test]
    fn test_tail_zero() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(10);
        buf.push(1);
        assert!(buf.tail(0).is_empty());
    }

    #[test]
    fn test_capacity_zero() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(0);
        buf.push(1);
        buf.push(2);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_extend() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(3);
        buf.extend(vec![1, 2, 3, 4, 5]);
        // 容量 3，应保留最后 3 个
        assert_eq!(buf.tail(10), vec![&5, &4, &3]);
    }

    #[test]
    fn test_clear() {
        let mut buf: RingBuffer<i32> = RingBuffer::new(3);
        buf.push(1);
        buf.push(2);
        buf.clear();
        assert!(buf.is_empty());
    }

    #[test]
    fn test_tail_cloned() {
        let mut buf: RingBuffer<String> = RingBuffer::new(3);
        buf.push("a".into());
        buf.push("b".into());
        let cloned = buf.tail_cloned(2);
        assert_eq!(cloned, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn test_default_capacity() {
        let buf: RingBuffer<i32> = RingBuffer::default();
        assert_eq!(buf.capacity(), 5000);
    }

    #[test]
    fn test_string_ring_buffer() {
        let mut buf: RingBuffer<String> = RingBuffer::new(2);
        buf.push("line 1".into());
        buf.push("line 2".into());
        buf.push("line 3".into()); // 淘汰 line 1
        let tail = buf.tail_cloned(10);
        assert_eq!(tail, vec!["line 3".to_string(), "line 2".to_string()]);
    }
}

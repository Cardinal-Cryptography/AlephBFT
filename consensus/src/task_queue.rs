use std::{cmp::Ordering, collections::BinaryHeap, time, time::Duration};

pub enum TaskResult {
    Cancel,
    Delay(Duration),
    Perform(Duration),
}

#[derive(Eq, PartialEq)]
struct ScheduledTask<T: Eq> {
    task: T,
    scheduled_time: time::Instant,
    // The number of times the task was performed so far.
    counter: usize,
}

impl<T: Eq> PartialOrd for ScheduledTask<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Eq> Ord for ScheduledTask<T> {
    /// Compare tasks so that earlier times come first in a max-heap.
    fn cmp(&self, other: &Self) -> Ordering {
        other.scheduled_time.cmp(&self.scheduled_time)
    }
}

pub struct TaskQueue<T: Eq + PartialEq> {
    queue: BinaryHeap<ScheduledTask<T>>,
}

impl<T: Eq> Default for TaskQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Eq> TaskQueue<T> {
    pub fn new() -> Self {
        Self {
            queue: BinaryHeap::new(),
        }
    }

    pub fn schedule_now(&mut self, task: T) {
        self.schedule(task, time::Instant::now());
    }

    pub fn schedule(&mut self, task: T, scheduled_time: time::Instant) {
        self.queue.push(ScheduledTask {
            task,
            scheduled_time,
            counter: 0,
        })
    }

    pub fn work_off<F: FnMut(&T, usize) -> TaskResult>(&mut self, mut processor: F) {
        while let Some(task) = self.queue.peek() {
            let curr_time = time::Instant::now();
            if task.scheduled_time > curr_time {
                break;
            }
            let mut task = self.queue.pop().expect("The element was peeked");

            match processor(&task.task, task.counter) {
                TaskResult::Cancel => (),
                TaskResult::Delay(delay) => {
                    task.scheduled_time += delay;
                    self.queue.push(task);
                }
                TaskResult::Perform(delay) => {
                    task.scheduled_time += delay;
                    task.counter += 1;
                    self.queue.push(task);
                }
            }
        }
    }
}

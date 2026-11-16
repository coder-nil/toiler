// 暂停与下一条提交共享屏障，不能把新消息放进即将被 abort 清掉的后端队列。
export class PauseHandoff {
  private pending: Promise<void> | null = null;
  private submission: symbol | null = null;

  get stopping() { return this.pending !== null; }
  get queued() { return this.submission !== null; }

  stop(operation: () => Promise<void>): Promise<void> {
    if (this.pending) return this.pending;
    const pending = Promise.resolve().then(operation).finally(() => {
      if (this.pending === pending) this.pending = null;
    });
    this.pending = pending;
    return pending;
  }

  cancelQueued() { this.submission = null; }

  async waitToSend(): Promise<boolean> {
    if (this.submission) return false;
    if (!this.pending) return true;
    const submission = Symbol();
    this.submission = submission;
    try {
      await this.pending;
      return this.submission === submission;
    } finally {
      if (this.submission === submission) this.submission = null;
    }
  }
}

"""Agent processes on several hosts, driven over ssh stdin/stdout."""
import json
import os
import queue
import subprocess
import threading
import time


def now_us():
    return time.time_ns() // 1000


class AgentError(RuntimeError):
    pass


class Agent:
    def __init__(self, name, command, log_dir):
        self.name = name
        self.stderr = open(os.path.join(log_dir, f'{name}.stderr'), 'ab')
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.stderr, bufsize=0)
        self.lock = threading.Lock()
        self.next_id = 0
        self.pending = {}
        self.events = queue.Queue()
        self.seen = {}
        self.seen_cond = threading.Condition()
        self.ready = threading.Event()
        self.offset_us = 0
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        if not self.ready.wait(120):
            raise AgentError(f'{name}: agent did not start')

    def _read(self):
        for raw in self.process.stdout:
            try:
                message = json.loads(raw)
            except ValueError:
                continue
            if 'id' in message:
                with self.lock:
                    slot = self.pending.pop(message['id'], None)
                if slot:
                    slot[1].update(message)
                    slot[0].set()
            elif message.get('event') == 'ready':
                self.ready.set()
            elif message.get('event') == 'seen':
                with self.seen_cond:
                    self.seen[message['seq']] = message
                    self.seen_cond.notify_all()
            else:
                self.events.put(message)
        with self.lock:
            for done, reply in self.pending.values():
                reply['err'] = 'agent exited'
                done.set()
            self.pending.clear()

    def start(self, op, **fields):
        with self.lock:
            self.next_id += 1
            ident = self.next_id
            done, reply = threading.Event(), {}
            self.pending[ident] = (done, reply)
        line = json.dumps({'id': ident, 'op': op, **fields}) + '\n'
        self.process.stdin.write(line.encode())
        self.process.stdin.flush()
        return Call(self, op, done, reply)

    def call(self, op, timeout=600, **fields):
        return self.start(op, **fields).result(timeout)

    def measure_offset(self, samples=15):
        best = None
        for _ in range(samples):
            sent = now_us()
            remote = self.call('clock', timeout=30)['now_us']
            received = now_us()
            rtt = received - sent
            offset = remote - (sent + received) // 2
            if best is None or rtt < best[0]:
                best = (rtt, offset)
        self.offset_us = best[1]
        return {'rtt_us': best[0], 'offset_us': best[1]}

    def to_driver(self, at_us):
        return at_us - self.offset_us

    def wait_seen(self, seq, deadline):
        with self.seen_cond:
            while seq not in self.seen:
                left = deadline - time.monotonic()
                if left <= 0:
                    return None
                self.seen_cond.wait(left)
            return self.seen[seq]

    def stop(self):
        try:
            self.process.stdin.close()
        except OSError:
            pass
        try:
            self.process.wait(60)
        except subprocess.TimeoutExpired:
            self.process.kill()


class Call:
    def __init__(self, agent, op, done, reply):
        self.agent, self.op, self.done, self.reply = agent, op, done, reply

    def result(self, timeout=600):
        if not self.done.wait(timeout):
            raise AgentError(f'{self.agent.name}: {self.op} timed out after {timeout}s')
        if 'err' in self.reply:
            raise AgentError(f'{self.agent.name}: {self.op}: {self.reply["err"]}')
        return self.reply['ok']

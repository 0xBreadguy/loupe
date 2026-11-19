#!/usr/bin/env python3
"""Synthetic end-to-end privacy and ask-count gate. Requires built release binaries."""
import json
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

import asks_lib as lib


def iso(offset_s):
    """Canonical T3 timestamp, offset from the watcher's wall clock."""
    moment = datetime.now(timezone.utc) + timedelta(seconds=offset_s)
    return moment.strftime('%Y-%m-%dT%H:%M:%S.000Z')


def watcher_signature(path):
    """mtime and size of the database and its -wal file, as status.rs `signature` reads them."""
    path = Path(path)

    def one(file):
        try:
            info = file.stat()
        except FileNotFoundError:
            return None
        return (info.st_mtime_ns, info.st_size)

    return (one(path), one(Path(str(path) + "-wal")))


class Privacy(lib.Base):
    def test_outputs_and_counts(self):
        fx = lib.Fx('privacy')
        fx.thread('pending')
        fx.run_row('pending', 'run', 'running')
        fx.message('pending', 'user', lib.stamp(29), body='A private human instruction.')
        fx.request('pending', 'req')
        fx.secret('pending', 'secret', 'run')
        fx.link('pending', 'link', 'run')
        fx.message('pending', 'assistant', lib.stamp(32), body='Do you prefer option one?')
        path = fx.close()
        for binary in ('term', 'core'):
            for mode in ('--status', '--once'):
                with self.subTest(binary=binary, mode=mode):
                    args = [mode, '--db', str(path)]
                    if mode == '--status':
                        args += ['--now-ms', str(lib.NOW_MS)]
                    code, out, err = lib.run(args, kind=binary)
                    self.assertEqual(code, 0, (out, err))
                    self.assertFalse(lib.leaks(out + err), 'private marker in output')
                    self.assertTrue(out.strip())
                    if mode == '--status':
                        status = json.loads(out)
                        self.assertGreater(status['waiting'], 0, status)
                        self.assertEqual(status['openAsks'], 3)
                        self.assertEqual(status['estimatedAsks'], 1)

    def test_lifecycle_outputs(self):
        fx = lib.Fx('lifecycle', wal=True)
        fx.thread('a')
        fx.run_row('a', 'run', 'running')
        fx.working('a', 'run')
        fx.request('a', 'request')
        fx.secret('a', 'secret', 'run')
        fx.link('a', 'link', 'run')
        fx.message('a', 'assistant', lib.stamp(32), body='Do you prefer option one?')
        for stage in range(3):
            for binary in ('term', 'core'):
                for mode in ('--status', '--once'):
                    args = [mode, '--db', str(fx.path)]
                    if mode == '--status':
                        args += ['--now-ms', str(lib.NOW_MS)]
                    code, out, err = lib.run(args, kind=binary)
                    self.assertEqual(code, 0, (stage, binary, mode, err))
                    self.assertFalse(lib.leaks(out + err), (stage, binary, mode))
                    if mode == '--status':
                        status = json.loads(out)
                        self.assertEqual(status['openAsks'], (3, 1, 0)[stage])
                        self.assertEqual(status['estimatedAsks'], (1, 1, 0)[stage])
            if stage == 0:
                fx.db.execute(f"UPDATE {lib.P}runtime_requests SET status='resolved', resolved_at='done'")
                fx.db.execute(f"UPDATE {lib.P}turn_items SET status='completed' WHERE turn_item_id='secret'")
                fx.event('a')
            elif stage == 1:
                fx.message('a', 'user', lib.stamp(33), body='Done.')
        fx.close()

    def recap(self, fx):
        code, out, err = lib.run(
            ['--recap', '--this-week', '--json', '--db', str(fx.path),
             '--now-ms', str(lib.NOW_MS)], T3_AGENTS_NO_THEMES='', TZ='UTC')
        self.assertEqual((code, err), (0, ''), out)
        self.assertFalse(lib.leaks(out), 'private marker in Recap')
        return json.loads(out)

    def trace(self, fx, run):
        code, out, err = lib.run(
            ['--trace', run, '--json', '--db', str(fx.path), '--now-ms', str(lib.NOW_MS)],
            T3_AGENTS_NO_THEMES='', TZ='UTC')
        self.assertEqual((code, err), (0, ''), out)
        self.assertFalse(lib.leaks(out), 'private marker in Trace')
        return json.loads(out)

    def test_recap_and_status_count_different_parts_of_an_ask_lifecycle(self):
        fx = lib.Fx('recap-lifecycle')
        self.addCleanup(fx.close)
        fx.thread('a')
        fx.run_row('a', 'run', 'running')
        fx.db.execute(f'UPDATE {lib.P}runs SET requested_at=?', (lib.T0,))
        fx.request('a', 'message', capability='message')
        fx.link('a', 'link', 'run')
        fx.message('a', 'assistant', lib.stamp(32), body='Do you prefer option one?')

        def counts():
            status, out = lib.status_json(fx.path)
            self.assertFalse(lib.leaks(out))
            return tuple(status[key] for key in ('running', 'waiting', 'openAsks', 'estimatedAsks'))

        self.assertEqual(counts(), (1, 0, 2, 1))
        self.assertEqual(self.trace(fx, 'run')['state'], 'running')
        self.assertEqual(self.recap(fx)['asks']['count'], 0)
        fx.db.execute(f"""UPDATE {lib.P}runs SET status='completed', completed_at=?,
                      payload_json=json_set(payload_json,'$.status','completed')""",
                      (lib.stamp(40),))
        fx.db.execute(f"UPDATE {lib.P}runtime_requests SET status='cancelled'")
        fx.message('a', 'user', lib.stamp(44), created_by='agent', body='Delivery.')
        self.assertEqual(counts(), (0, 1, 1, 1))
        self.assertEqual(self.trace(fx, 'run')['state'], 'waiting')
        self.assertEqual(self.recap(fx)['asks']['count'], 1)
        fx.message('a', 'user', lib.stamp(50), body='Done.')
        self.assertEqual(counts(), (0, 0, 0, 0))
        trace = self.trace(fx, 'run')
        self.assertEqual(trace['state'], 'completed')
        self.assertEqual(trace['split']['waiting'], 10 * 60_000)
        recap = self.recap(fx)
        self.assertEqual(recap['asks']['count'], 1)
        self.assertEqual(recap['asks']['typical_ms'], 10 * 60_000)
        self.assertAlmostEqual(recap['split']['waiting'], 10 / 60)

    def test_recap_counts_only_waiting_link_cards_as_open(self):
        fx = lib.Fx('recap-link-state')
        self.addCleanup(fx.close)
        fx.thread('a')
        fx.run_row('a', 'run', 'completed')
        fx.link('a', 'link', 'run')
        fx.db.execute(f"UPDATE {lib.P}turn_items SET status='running'")
        self.assertEqual(lib.status_json(fx.path)[0]['openAsks'], 0)
        self.assertEqual(self.recap(fx)['asks']['count'], 0)

    def test_recap_secret_needs_an_eligible_owning_run(self):
        fx = lib.Fx('recap-secret-run')
        self.addCleanup(fx.close)
        fx.thread('a')
        fx.run_row('a', 'queued', 'queued')
        fx.run_row('a', 'working', 'running', ordinal=2)
        fx.secret('a', 'secret', 'queued')
        self.assertEqual(lib.status_json(fx.path)[0]['openAsks'], 0)
        self.assertEqual(self.recap(fx)['asks']['count'], 0)

    def test_watch_keeps_ask_counts_across_commits(self):
        """One real `t3-agents-term --status --watch` process, across committed updates.

        Starts on an active thread with a message-mode formal ask and one completed
        reader-directed question. A non-human delivery is committed in the same
        transaction as a title change, so the watcher has to emit that snapshot:
        the delivery itself changes no status field, and the title is how this
        test knows the emission is that commit. Resolving the request then drops
        the exact count by one. A later human reply drops the estimate.
        """
        fx = lib.Fx('watch', wal=True)
        fx.db.execute('PRAGMA busy_timeout=5000')
        assistant_at = iso(-90)
        delivery_at = iso(-30)
        reply_at = iso(-1)
        fx.thread('a')
        fx.run_row('a', 'run', 'running')
        fx.working('a', 'run', at=iso(-20))
        fx.request('a', 'req', capability='message')
        fx.message('a', 'assistant', assistant_at, body='Do you prefer option one?')
        watch = lib.StatusWatch(fx.path)
        try:
            self.assert_watch_line(watch.next_line(), 1, 1, 'Thread a')
            self.assertIsNone(watch.proc.poll())

            def deliver():
                fx.message('a', 'user', delivery_at, created_by='agent',
                           body='Automatic delivery. Should I continue?')
                fx.db.execute(
                    f"UPDATE {lib.P}threads SET payload_json=json_set(payload_json,'$.title',?) "
                    "WHERE thread_id='a'",
                    ('Thread a, still working',),
                )

            self.commit(fx, deliver)
            delivered = watch.next_line()
            self.assert_watch_line(delivered, 1, 1, 'Thread a, still working')
            self.assertEqual(watch.collect(1), [], 'delivery emitted a second line')

            def resolve():
                fx.db.execute(
                    f"UPDATE {lib.P}runtime_requests SET status='resolved', resolved_at='done' "
                    "WHERE runtime_request_id='req'")
                fx.event('a')

            self.commit(fx, resolve)
            self.assert_watch_line(watch.next_line(), 0, 1, 'Thread a, still working')

            def reply():
                fx.message('a', 'user', reply_at, body='Done.')

            self.commit(fx, reply)
            self.assert_watch_line(watch.next_line(), 0, 0, 'Thread a, still working')
            self.assertEqual(watch.collect(1), [], 'human reply emitted a second line')
            self.assertTrue(watch.alive(), watch.stderr())
            self.assertEqual(watch.pid, watch.proc.pid)
        finally:
            watch.stop()
            fx.close()
        self.assertFalse(watch.alive())
        self.assertFalse(lib.leaks(watch.output()), watch.output())
        self.assertEqual(watch.stderr(), '')

    def commit(self, fx, writes):
        before = watcher_signature(fx.path)
        fx.db.execute('BEGIN IMMEDIATE')
        try:
            writes()
        except Exception:
            fx.db.execute('ROLLBACK')
            raise
        else:
            fx.db.execute('COMMIT')
        after = watcher_signature(fx.path)
        self.assertNotEqual(before, after, 'commit did not change the database signature the watcher reads')

    def assert_watch_line(self, line, open_asks, estimated, title):
        self.assertFalse(lib.leaks(line), line)
        status = json.loads(line)
        self.assertNotIn('error', status, status)
        self.assertIsInstance(status['running'], int)
        self.assertIsInstance(status['waiting'], int)
        self.assertIsInstance(status['openAsks'], int)
        self.assertIsInstance(status['estimatedAsks'], int)
        # Message mode does not block, so the thread stays in the running count.
        self.assertEqual(status['running'], 1, status)
        self.assertEqual(status['waiting'], 0, status)
        self.assertEqual(len(status['agents']), 1, status)
        agent = status['agents'][0]
        self.assertEqual(agent['title'], title, status)
        self.assertEqual(agent['state'], 'running', status)
        self.assertEqual(agent['now'], 'shell', status)
        self.assertIsInstance(agent['openAsks'], int)
        self.assertIsInstance(agent['estimatedAsks'], int)
        self.assertEqual(agent['openAsks'], open_asks, status)
        self.assertEqual(status['openAsks'], open_asks, status)
        self.assertEqual(agent['estimatedAsks'], estimated, status)
        self.assertEqual(status['estimatedAsks'], estimated, status)

    def test_leak_detector_controls(self):
        self.assertTrue(lib.leaks(lib.MSG_MARK))
        self.assertTrue(lib.leaks(lib.PRIV_MARK))
        self.assertFalse(lib.leaks('ordinary output'))


if __name__ == '__main__':
    lib.main(__name__)

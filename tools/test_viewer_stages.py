"""Independent stage partition and incomplete/failing fence rejection."""
import copy
import unittest
from analyze_viewer_stages import analyze, FIELDS, SIGNALED, TIMEOUT, SATISFIED


def records():
    points = [100, 102, 105, 105, 106, 110, 111, 124, 125, 126, 130]
    row = dict(event='stage', frame=1, wait_calls=3, timeouts=2,
               first_status=TIMEOUT, final_status=SATISFIED, gpu_bracket_ns=0)
    row.update({name+'_ns': value*1000000 for name, value in zip(FIELDS, points)})
    return [row, dict(event='summary', frames=1, clock='CLOCK_MONOTONIC',
                      limit=32768, writing='after-rendering')]


class ViewerStageTests(unittest.TestCase):
    def test_stages_partition_elapsed_cpu_time(self):
        result = analyze(records(), 1)
        self.assertEqual(result['total_ms']['p50'], 30)
        self.assertEqual(result['cpu_stage_ms']['fence_wait']['p50'], 13)
        self.assertEqual(sum(row['mean'] for row in result['cpu_stage_ms'].values()), 30)

    def test_incomplete_reordered_and_count_mismatches_fail(self):
        for mutate in [lambda rows: rows.pop(),
                       lambda rows: rows[0].update(draw_ns=1),
                       lambda rows: rows[0].update(frame=2),
                       lambda rows: rows[-1].update(frames=2),
                       lambda rows: rows[-1].update(clock='gpu')]:
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                rows=copy.deepcopy(records()); mutate(rows); analyze(rows)
        with self.assertRaises(ValueError):
            analyze(records(), 2)

    def test_timeout_or_failed_wait_cannot_be_a_completed_frame(self):
        for update in [dict(final_status=TIMEOUT), dict(final_status=0x911d),
                       dict(wait_calls=2), dict(first_status=SIGNALED), dict(timeouts=-1)]:
            with self.subTest(update=update), self.assertRaises(ValueError):
                rows=records(); rows[0].update(update); analyze(rows)

    def test_already_signaled_proof_is_preserved(self):
        rows=records()
        rows[0].update(wait_calls=1, timeouts=0, first_status=SIGNALED, final_status=SIGNALED)
        self.assertEqual(analyze(rows)['fences_already_signaled_at_first_wait'], 1)

    def test_gpu_duration_is_separate_and_cannot_be_partially_missing(self):
        rows=records(); rows[0]['gpu_bracket_ns']=15000
        result=analyze(rows)
        self.assertEqual(result['gpu_bracket_us']['p50'], 15)
        self.assertEqual(result['cpu_stage_ms']['fence_wait']['p50'], 13)
        later=copy.deepcopy(rows[0]); later.update(frame=2, gpu_bracket_ns=0)
        for field in FIELDS:
            later[field+'_ns']+=1000000000
        rows.insert(1,later); rows[-1]['frames']=2
        with self.assertRaises(ValueError):
            analyze(rows)


if __name__ == '__main__':
    unittest.main()

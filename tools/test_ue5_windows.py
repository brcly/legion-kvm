import unittest
from analyze_ue5_frames import windows


class FrameWindowTests(unittest.TestCase):
    def test_frame_index_is_not_used_as_elapsed_time(self):
        result=windows('Time,FPS\n0,100\n1,100\n2,25\n3,200\n',.025)
        self.assertAlmostEqual(result['estimated_trace_seconds'],.065)
        self.assertEqual(result['slow_frames']['over_33.3333_ms'],1)
        self.assertEqual([row['frames'] for row in result['windows']],[3,1])
        self.assertEqual([row['first_frame'] for row in result['windows']],[0,3])

    def test_invalid_or_truncated_source_cannot_qualify(self):
        for text in ['Time,FPS\n0,0\n','Time,FPS\n1,100\n','Time,FPS\n0,100']:
            with self.subTest(text=text),self.assertRaises(ValueError):windows(text)
        with self.assertRaises(ValueError):windows('Time,FPS\n0,100\n',0)

    def test_steady_subset_excludes_long_initial_frame_by_elapsed_time(self):
        result=windows('Time,FPS\n0,0.1\n1,100\n2,25\n')
        steady=result['after_estimated_10_seconds']
        self.assertEqual(steady['frames'],2)
        self.assertEqual(steady['frame_period_fps'],40)
        self.assertAlmostEqual(steady['p99_ms'],39.7)
        self.assertEqual(steady['over_25_ms_frames'],1)


if __name__=='__main__':unittest.main()

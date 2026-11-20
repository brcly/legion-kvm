"""Measurement validity checks, independent of GPU performance observations."""
import copy
import unittest
from measure_input_latency import summarize


class ObserverValidation(unittest.TestCase):
    def fixture(self):
        events = [dict(event='fixture_ready', magic=0xa5c3, time_ns=1)]
        for index in range(1, 21):
            start=index*1_000_000_000
            events.append(dict(event='sample', sequence=index, injected_ns=start,
                               observed_ns=start+5_000_000, capture_requested_ns=start+1_000_000,
                               response_ms=5., capture_ms=4.))
        events.append(dict(event='summary', passed=True, samples=20))
        return events

    def test_requires_a_changed_frame_for_each_unique_input(self):
        events=self.fixture()
        self.assertEqual(summarize(events,20)['response_ms']['p50'],5.)
        for mutate in [lambda e:e[5].update(sequence=4),
                       lambda e:e[5].update(observed_ns=e[5]['injected_ns']-1),
                       lambda e:e[5].update(capture_requested_ns=-1),
                       lambda e:e[5].update(capture_ms=float('nan')),
                       lambda e:e[-1].update(passed=False),
                       lambda e:e.pop(5)]:
            bad=copy.deepcopy(events);mutate(bad)
            with self.assertRaises(ValueError):summarize(bad,20)

    def test_rejects_claimed_latency_not_matching_host_timestamps(self):
        events=self.fixture();events[3]['response_ms']=.001
        with self.assertRaises(ValueError):summarize(events,20)
        events=self.fixture();events[3]['capture_ms']=.001
        with self.assertRaises(ValueError):summarize(events,20)


class RuntimeFeedbackValidation(unittest.TestCase):
    def fixture(self):
        events=[dict(event='fixture_ready',magic=0xa5c3,time_ns=1),
                dict(event='runtime_ready',qpc_frequency=10_000_000)]
        for index in range(1,21):
            start=index*1_000_000_000
            qpc=index*100
            events.append(dict(event='sample',sequence=index,injected_ns=start,observed_ns=start+5_000_000,
                response_ms=5.,target_present_id=index*10,feedback_present_id=index*10+1,
                source_input_qpc=qpc,source_submitted_qpc=qpc+1,source_feedback_qpc=qpc+20,source_polled_qpc=qpc+30,
                fence_value=index*20,completed_fence_value=index*20+1,
                source_input_to_feedback_ms=.002,source_feedback_to_poll_ms=.001))
        events.extend([dict(event='final_image_verified',sequence=20,time_ns=events[-1]['observed_ns']+1),
                       dict(event='runtime_measurement',timed_captures=0),dict(event='summary',passed=True,samples=20)])
        return events

    def test_runtime_method_requires_frame_identity_original_fence_and_both_images(self):
        events=self.fixture()
        self.assertEqual(summarize(events,20)['method'],'runtime_feedback')
        mutations=[lambda e:e[6].update(target_present_id=1),
                   lambda e:e[6].update(feedback_present_id=1),
                   lambda e:e[6].update(completed_fence_value=1),
                   lambda e:e[6].update(completed_fence_value=2**64-1),
                   lambda e:e[6].update(source_feedback_qpc=1),
                   lambda e:e[6].update(source_input_to_feedback_ms=50.),
                   lambda e:e[-2].update(timed_captures=1),
                   lambda e:e.pop(-3)]
        for mutate in mutations:
            bad=copy.deepcopy(events);mutate(bad)
            with self.assertRaises(ValueError):summarize(bad,20)


if __name__=='__main__':unittest.main()

"""Reject incorrect frame identity joins independently of measured latency."""
import copy
import unittest
from link_frame_presentation import link


def fixture():
    records=[dict(event='configuration',clock_id=1,surface=47)]
    lines=[]
    for frame,generation,begin in [(1,0,100),(2,1,220),(3,1,300),(4,2,420)]:
        presented=frame!=2
        records.append(dict(event='frame',frame=frame,feedback_object=66,status=1 if presented else 2,
            draw_ns=begin,gpu_done_ns=begin+10,swap_begin_ns=begin+20,swap_return_ns=begin+30,
            received_ns=begin+60,presented_ns=begin+50 if presented else 0,flags=0,
            tag_valid=1,input_generation=generation,source_frame_id=frame))
        lines.extend([f'LEGION_SWAP_BEGIN frame={frame} surface=47',
                      ' -> wp_presentation#63.feedback(wl_surface#47, new id wp_presentation_feedback#66)',
                      ' -> wl_surface#47.attach(wl_buffer#99, 0, 0)',
                      ' -> wl_surface#47.commit()',f'LEGION_SWAP_END frame={frame} surface=47'])
    source=dict(frame_tags=True,original_queue_drained=True,gpu_verified=True,
                tagged_frames=[[1,0,1,1],[2,1,2,2],[3,1,3,3],[4,2,4,4]])
    events=[dict(event='sample',sequence=1,injected_ns=200,observed_ns=280,target_present_id=2,fence_value=2),
            dict(event='sample',sequence=2,injected_ns=400,observed_ns=480,target_present_id=4,fence_value=4),
            dict(event='final_image_verified',capture_requested_ns=600,time_ns=650)]
    return events,source,records,lines


class LinkTests(unittest.TestCase):
    def test_first_presented_response_skips_discarded_source_frame(self):
        result=link(*fixture())
        self.assertEqual(result['samples'][0]['source_frame_id'],3)
        self.assertEqual(result['samples'][0]['discarded_responsive_updates'],1)
        self.assertEqual(result['samples'][0]['presented_ns'],350)

    def test_rejects_incorrect_source_and_input_identity(self):
        changes=[lambda v:v[2][3].update(source_frame_id=99),
                 lambda v:v[2][3].update(input_generation=2),
                 lambda v:v[2][3].update(tag_valid=0),
                 lambda v:v[1].update(frame_tags=False),
                 lambda v:v[1]['tagged_frames'][2].__setitem__(2,1),
                 lambda v:v[0][0].update(target_present_id=4),
                 lambda v:v[0][0].update(fence_value=4),
                 lambda v:v[0][0].update(injected_ns=351),
                 lambda v:v[0][0].update(control_started_ns=201),
                 lambda v:v[0][0].update(control_started_ns=180,dispatch_delay_ns=99),
                 lambda v:v[0][-1].update(capture_requested_ns=440)]
        for change in changes:
            value=copy.deepcopy(fixture());change(value)
            with self.subTest(change=change),self.assertRaises(ValueError):link(*value)

    def test_calibration_keeps_actual_dispatch_endpoint(self):
        value=fixture();value[0][0].update(control_started_ns=180,dispatch_delay_ns=20)
        sample=link(*value)['samples'][0]
        self.assertEqual(sample['input_to_presented_ms'],.00015)
        self.assertEqual(sample['control_to_presented_ms'],.00017)

    def test_unpresented_response_and_bad_commit_cannot_count(self):
        value=fixture();value[2][3].update(status=2,presented_ns=0)
        with self.assertRaises(ValueError):link(*value)
        value=fixture();value[3].pop(3)
        with self.assertRaises(ValueError):link(*value)


if __name__=='__main__':unittest.main()

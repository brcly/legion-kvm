"""Reject ambiguous swap ownership and unusable presentation evidence."""
import copy
import unittest

from audit_host_presentation import audit


def fixture():
    events=[dict(event='configuration',clock_id=1,surface=47)]
    lines=[]
    for frame in (1,2):
        events.append(dict(event='frame',frame=frame,feedback_object=66,status=frame,
                           draw_ns=100,gpu_done_ns=200,swap_begin_ns=210,
                           swap_return_ns=250,received_ns=400,
                           presented_ns=300 if frame==1 else 0,flags=5 if frame==1 else 0))
        lines.extend([f'LEGION_SWAP_BEGIN frame={frame} surface=47',
                      ' -> wp_presentation#63.feedback(wl_surface#47, new id wp_presentation_feedback#66)',
                      ' -> wl_surface#47.attach(wl_buffer#99, 0, 0)',
                      ' -> wl_surface#47.commit()',
                      f'LEGION_SWAP_END frame={frame} surface=47'])
    return events,lines


class AuditTests(unittest.TestCase):
    def early_fixture(self):
        events,old=fixture();events[0]['early_swap']=1
        lines=[' -> wp_linux_drm_syncobj_manager_v1#65.get_surface(new id wp_linux_drm_syncobj_surface_v1#70, wl_surface#47)',
               ' -> wp_linux_drm_syncobj_manager_v1#65.import_timeline(new id wp_linux_drm_syncobj_timeline_v1#71, fd 10)',
               ' -> wp_linux_drm_syncobj_manager_v1#65.import_timeline(new id wp_linux_drm_syncobj_timeline_v1#72, fd 11)']
        for frame in (1,2):
            events[frame].update(early_swap=1,draw_ns=100,swap_begin_ns=120,swap_return_ns=130,gpu_done_ns=250,
                                 presented_ns=200 if frame==1 else 0)
            lines+=old[(frame-1)*5:(frame-1)*5+2]+[
                f' -> wp_linux_drm_syncobj_surface_v1#70.set_acquire_point(wp_linux_drm_syncobj_timeline_v1#71, 0, {frame})',
                f' -> wp_linux_drm_syncobj_surface_v1#70.set_release_point(wp_linux_drm_syncobj_timeline_v1#72, 0, {frame})',
                *old[(frame-1)*5+2:frame*5]]
        return events,lines

    def test_early_swap_keeps_cpu_fence_observation_distinct_from_presentation(self):
        events,lines=self.early_fixture();result=audit(events,lines)
        self.assertTrue(result['explicit_sync_audited'])
        self.assertIsNone(result['gpu_done_to_presented_ms'])
        self.assertEqual(result['fence_observed_to_presented_signed_ms']['p50'],-.00005)

    def test_early_swap_requires_actual_owned_explicit_sync(self):
        events,lines=self.early_fixture()
        changes=[lambda a:[line for line in a if '.set_acquire_point' not in line],
                 lambda a:[line for line in a if '.set_release_point' not in line],
                 lambda a:[line.replace('wl_surface#47)','wl_surface#48)') for line in a],
                 lambda a:[line.replace('timeline_v1#71, 0,','timeline_v1#91, 0,') for line in a],
                 lambda a:[line.replace('timeline_v1#71, 0, 2','timeline_v1#71, 0, 1') for line in a],
                 lambda a:[line.replace('timeline_v1#71, 0, 1','timeline_v1#71, 0, 0') for line in a],
                 lambda a:[line.replace('timeline_v1#72,','timeline_v1#71,') for line in a]]
        for change in changes:
            with self.subTest(change=change),self.assertRaises(ValueError):audit(events,change(lines))
        bad=copy.deepcopy(events);bad[1]['gpu_done_ns']=129
        with self.assertRaises(ValueError):audit(bad,lines)
        bad=copy.deepcopy(events);bad[1]['early_swap']=0
        with self.assertRaises(ValueError):audit(bad,lines)

    def test_presented_discarded_and_reused_object(self):
        events,lines=fixture()
        result=audit(events,lines)
        self.assertEqual((result['audited_swaps'],result['presented'],result['discarded']),(2,1,1))
        self.assertEqual(result['gpu_done_to_presented_ms']['p50'],.0001)

    def test_rejects_protocol_ambiguity(self):
        events,lines=fixture()
        for label,changed in [
            ('duplicate commit',lines[:4]+[lines[3]]+lines[4:]),
            ('missing commit',lines[:3]+lines[4:]),
            ('late feedback',[lines[0],lines[2],lines[1]]+lines[3:]),
            ('outside attach',[lines[2]]+lines),
            ('wrong surface',[line.replace('surface=47','surface=48') for line in lines]),
            ('wrong feedback',[line.replace('feedback#66','feedback#67') for line in lines]),
            ('missing swap',lines[:5]),
            ('duplicate swap',lines+lines[:5]),
        ]:
            with self.subTest(label=label),self.assertRaises(ValueError):audit(events,changed)

    def test_other_surfaces_and_at_object_format(self):
        events,lines=fixture()
        lines=lines[:3]+[' -> wl_surface#98.attach(wl_buffer#90, 0, 0)',
                         ' -> wl_surface#98.commit()']+lines[3:]
        result=audit(events,[line.replace('#','@') for line in lines])
        self.assertEqual(result['audited_swaps'],2)

    def test_rejects_invalid_records(self):
        events,lines=fixture()
        mutations=[(0,'clock_id',0),(1,'frame',2),(1,'status',3),
                   (1,'presented_ns',199),(1,'presented_ns',401),
                   (1,'swap_return_ns',401),(1,'gpu_done_ns',99),
                   (2,'presented_ns',300),(2,'flags',1)]
        for index,key,value in mutations:
            changed=copy.deepcopy(events);changed[index][key]=value
            with self.subTest(index=index,key=key,value=value),self.assertRaises(ValueError):audit(changed,lines)

    def test_all_discarded_is_not_presentation(self):
        events,lines=fixture();events[1].update(status=2,presented_ns=0,flags=0)
        with self.assertRaises(ValueError):audit(events,lines)


if __name__=='__main__':unittest.main()

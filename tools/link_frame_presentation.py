"""Join rendered fixture tags to audited host commits using host timestamps only."""
import math
import statistics
from audit_host_presentation import audit


def link(events, source, records, protocol):
    audited=audit(records,protocol)
    samples=[e for e in events if e.get('event')=='sample']
    final=[e for e in events if e.get('event')=='final_image_verified']
    if (not samples or len(final)!=1 or not source.get('frame_tags')
            or not source.get('original_queue_drained') or not source.get('gpu_verified')):
        raise ValueError('missing source tag capability, original drain or independent final image')
    cutoff=final[0].get('capture_requested_ns',0)
    if cutoff<=samples[-1]['observed_ns']:
        raise ValueError('final capture overlaps host presentation observation')
    mapping={};last_present=last_fence=0
    for expected,row in enumerate(source.get('tagged_frames',[]),1):
        if len(row)!=4:raise ValueError('invalid source frame record')
        frame,generation,present,fence=row
        if frame!=expected or not 0<=generation<=len(samples) or present<=last_present or fence<=last_fence:
            raise ValueError('invalid source frame/present/fence identity')
        mapping[frame]=(generation,present,fence);last_present,last_fence=present,fence
    frames=[row for row in records if row.get('event')=='frame' and row.get('tag_valid')]
    if not mapping or not frames:raise ValueError('no independently decoded source frame tags')
    previous_frame=0
    for row in frames:
        frame=row['source_frame_id'];generation=row['input_generation']
        if frame<previous_frame or frame not in mapping or mapping[frame][0]!=generation:
            raise ValueError('decoded frame tag is stale, unknown or belongs to another input')
        previous_frame=frame
    results=[]
    for index,sample in enumerate(samples):
        generation=index+1
        if sample['sequence']!=generation:raise ValueError('input sequence missing or reordered')
        candidates=[row for row in frames if row['input_generation']==generation and row['status']==1]
        if not candidates:raise ValueError('input generation was never presented by host compositor')
        first=min(candidates,key=lambda row:row['presented_ns'])
        limit=samples[index+1]['injected_ns'] if index+1<len(samples) else cutoff
        present,fence=mapping[first['source_frame_id']][1:]
        control=sample.get('control_started_ns',sample['injected_ns'])
        if not 0<control<=sample['injected_ns'] or sample.get('dispatch_delay_ns',sample['injected_ns']-control)!=sample['injected_ns']-control:
            raise ValueError('invalid host dispatch calibration clock')
        if not sample['injected_ns']<first['draw_ns']<=first['presented_ns']<limit:
            raise ValueError('response predates input, arrives after next input or overlaps final capture')
        if present<sample['target_present_id'] or fence<sample['fence_value']:
            raise ValueError('presented frame predates original source target/fence')
        # No invalid-tag update may hide an earlier response in this interval.
        if any(not row.get('tag_valid') and row.get('status')==1
               and sample['injected_ns']<=row['presented_ns']<=first['presented_ns']
               for row in records if row.get('event')=='frame'):
            raise ValueError('unidentified presented image overlaps response observation')
        results.append(dict(sequence=generation,source_frame_id=first['source_frame_id'],
            source_present_id=present,source_fence=fence,viewer_update=first['frame'],
            injected_ns=sample['injected_ns'],presented_ns=first['presented_ns'],
            input_to_presented_ms=(first['presented_ns']-sample['injected_ns'])/1e6,
            control_to_presented_ms=(first['presented_ns']-control)/1e6,
            dispatch_delay_ms=(sample['injected_ns']-control)/1e6,
            input_to_viewer_draw_ms=(first['draw_ns']-sample['injected_ns'])/1e6,
            viewer_draw_to_gpu_done_ms=(first['gpu_done_ns']-first['draw_ns'])/1e6,
            viewer_start_to_presented_ms=(first['presented_ns']-first['draw_ns'])/1e6,
            host_gpu_to_presented_ms=(first['presented_ns']-first['gpu_done_ns'])/1e6,
            discarded_responsive_updates=sum(row['input_generation']==generation and row['status']==2 for row in frames)))
    def distribution(values):
        values=sorted(values);position=(len(values)-1)*.95;lo,hi=math.floor(position),math.ceil(position)
        return dict(p50=statistics.median(values),p95=values[lo]+(values[hi]-values[lo])*(position-lo),maximum=max(values))
    result=dict(schema=1,passed=True,sample_count=len(results),samples=results,audit=audited,
        decoded_viewer_updates=len(frames),source_frame_count=len(mapping),
        response_ms=distribution([row['input_to_presented_ms'] for row in results]),
        control_response_ms=distribution([row['control_to_presented_ms'] for row in results]),
        input_to_viewer_draw_ms=distribution([row['input_to_viewer_draw_ms'] for row in results]),
        viewer_draw_to_gpu_done_ms=distribution([row['viewer_draw_to_gpu_done_ms'] for row in results]),
        host_gpu_to_presented_ms=distribution([row['host_gpu_to_presented_ms'] for row in results]),
        tag_readback_bytes_per_viewer_update=8,
        scope='host virtual-keyboard dispatch to first audited compositor-presented tagged source response',
        limitations=['Instrumented pipeline: eight decoded bytes read back per viewer update, plus Wayland protocol audit logging',
                     'Private headless compositor feedback is not monitor scanout or optical input-to-photon',
                     'Exact fixture frame tags map to source Present IDs/fences; this does not instrument arbitrary games',
                     'All latency endpoints use host CLOCK_MONOTONIC; source QPC absolute timestamps are never subtracted'])
    result['early_swap']=audited.get('early_swap',False)
    if result['early_swap']:
        for row in results:row['fence_observed_to_presented_signed_ms']=row.pop('host_gpu_to_presented_ms')
        result['fence_observed_to_presented_signed_ms']=result.pop('host_gpu_to_presented_ms')
        result['limitations'].append('Fence completion is CPU-observed after swap; signed observation-to-presentation offset is not a GPU-ready latency')
    return result

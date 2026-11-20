#!/usr/bin/env python3
"""Audit viewer swaps against actual Wayland requests before assigning timestamps."""
import argparse
import json
import math
from pathlib import Path
import re
import statistics

OBJECT = r'(?:#|@)(\d+)'
BEGIN = re.compile(r'^LEGION_SWAP_BEGIN frame=(\d+) surface=(\d+)$')
END = re.compile(r'^LEGION_SWAP_END frame=(\d+) surface=(\d+)$')
COMMIT = re.compile(r'->\s+wl_surface'+OBJECT+r'\.commit\(')
ATTACH = re.compile(r'->\s+wl_surface'+OBJECT+r'\.attach\(wl_buffer'+OBJECT)
FEEDBACK = re.compile(r'->\s+wp_presentation'+OBJECT+r'\.feedback\(wl_surface'+OBJECT+r',\s*new id wp_presentation_feedback'+OBJECT)

SYNC_SURFACE = re.compile(r'->\s+wp_linux_drm_syncobj_manager_v1'+OBJECT+r'\.get_surface\(new id wp_linux_drm_syncobj_surface_v1'+OBJECT+r',\s*wl_surface'+OBJECT)
TIMELINE = re.compile(r'->\s+wp_linux_drm_syncobj_manager_v1'+OBJECT+r'\.import_timeline\(new id wp_linux_drm_syncobj_timeline_v1'+OBJECT)
DESTROY_TIMELINE = re.compile(r'->\s+wp_linux_drm_syncobj_timeline_v1'+OBJECT+r'\.destroy\(')
POINT = re.compile(r'->\s+wp_linux_drm_syncobj_surface_v1'+OBJECT+r'\.set_(acquire|release)_point\(wp_linux_drm_syncobj_timeline_v1'+OBJECT+r',\s*(\d+),\s*(\d+)\)')


def audit(events, protocol_lines):
    config = [row for row in events if row.get('event')=='configuration']
    frames = [row for row in events if row.get('event')=='frame']
    if len(config)!=1 or config[0].get('clock_id')!=1 or not frames:
        raise ValueError('missing host presentation capability or unsupported clock')
    surface = config[0]['surface']
    early=bool(config[0].get('early_swap',False))
    sync_surfaces={};timelines={};points={}
    expected = {row['frame']:row for row in frames}
    if len(expected)!=len(frames) or sorted(expected)!=list(range(1,len(frames)+1)):
        raise ValueError('viewer frame IDs are missing or duplicated')
    active=None; seen=set(); feedback=None; attached=False; committed=False
    for line in protocol_lines:
        line=line.strip()
        begin=BEGIN.match(line)
        end=END.match(line)
        if begin:
            frame, target=map(int,begin.groups())
            if active is not None or target!=surface or frame not in expected or frame in seen:
                raise ValueError('overlapping, unknown or wrong-surface viewer swap')
            active=frame;feedback=None;attached=committed=False;points={}
        elif end:
            frame,target=map(int,end.groups())
            if active!=frame or target!=surface or not attached or not committed or feedback!=expected[frame]['feedback_object']:
                raise ValueError('swap has no unique matching feedback, attach and commit')
            if early and set(points)!= {'acquire','release'}:raise ValueError('early swap lacks audited explicit acquire/release points')
            if early and points['acquire'][0]==points['release'][0] and points['release'][1]<=points['acquire'][1]:
                raise ValueError('early swap has conflicting synchronization points')
            seen.add(frame);active=None
        elif early and (match:=SYNC_SURFACE.search(line)):
            _,obj,target=map(int,match.groups());sync_surfaces[obj]=target
        elif early and (match:=TIMELINE.search(line)):
            _,obj=map(int,match.groups());timelines[obj]=0
        elif early and (match:=DESTROY_TIMELINE.search(line)):
            timelines.pop(int(match[1]),None)
        elif early and (match:=POINT.search(line)):
            obj,kind,timeline,hi,lo=match.groups();obj,timeline,hi,lo=map(int,[obj,timeline,hi,lo])
            if sync_surfaces.get(obj)==surface:
                value=(hi<<32)|lo
                if active is None or committed or kind in points or timeline not in timelines or not value or hi>0xffffffff or lo>0xffffffff:
                    raise ValueError('early swap synchronization point is unknown, duplicate or late')
                if kind=='acquire' and value<=timelines[timeline]:raise ValueError('early swap acquire point was reused')
                if kind=='acquire':timelines[timeline]=value
                points[kind]=(timeline,value)
        elif match:=FEEDBACK.search(line):
            _,target,object_id=map(int,match.groups())
            if target==surface:
                if active is None or feedback is not None or committed or attached:
                    raise ValueError('feedback request is outside or late in its swap')
                feedback=object_id
        elif match:=ATTACH.search(line):
            target,_=map(int,match.groups())
            if target==surface:
                if active is None or attached or committed or feedback is None:
                    raise ValueError('buffer attach is not uniquely inside its swap')
                attached=True
        elif match:=COMMIT.search(line):
            target=int(match[1])
            if target==surface and active is not None:
                if committed or not attached or feedback is None:
                    raise ValueError('unexpected intermediate or duplicate surface commit')
                committed=True
    if active is not None or seen!=set(expected):
        raise ValueError('viewer swaps were not all audited')
    presented=[];discarded=0
    for row in frames:
        if row['status'] not in (1,2):
            raise ValueError('unresolved/abandoned presentation feedback')
        if bool(row.get('early_swap',False))!=early:raise ValueError('mixed swap ordering in presentation records')
        order=([row[key] for key in ['draw_ns','swap_begin_ns','swap_return_ns','gpu_done_ns','received_ns']] if early else [row[key] for key in ['draw_ns','gpu_done_ns','swap_begin_ns','swap_return_ns','received_ns']])
        if not order[0]>0 or order!=sorted(order):
            raise ValueError('viewer clock ordering is invalid')
        if row['status']==1:
            if not row['swap_begin_ns' if early else 'gpu_done_ns']<=row['presented_ns']<=row['received_ns']:
                raise ValueError('compositor presentation clock is not causally usable')
            presented.append(row)
        else:
            if row['presented_ns'] or row.get('flags',0):
                raise ValueError('discarded feedback carries a presentation timestamp or flags')
            discarded+=1
    if not presented:
        raise ValueError('compositor did not present any audited viewer frame')
    def summary(values):
        values=sorted(values)
        x=(len(values)-1)*.95;lo,hi=math.floor(x),math.ceil(x)
        return dict(p50=statistics.median(values),p95=values[lo]+(values[hi]-values[lo])*(x-lo),maximum=max(values))
    flags={str(value):sum(row['flags']==value for row in presented) for value in sorted({r['flags'] for r in presented})}
    return dict(schema=1,passed=True,audited_swaps=len(seen),presented=len(presented),discarded=discarded,
                clock_id=1,flag_counts=flags,early_swap=early,explicit_sync_audited=early,
                gpu_done_to_presented_ms=None if early else summary([(r['presented_ns']-r['gpu_done_ns'])/1e6 for r in presented]),
                fence_observed_to_presented_signed_ms=summary([(r['presented_ns']-r['gpu_done_ns'])/1e6 for r in presented]) if early else None,
                gpu_done_to_swap_begin_ms=None if early else summary([(r['swap_begin_ns']-r['gpu_done_ns'])/1e6 for r in presented]),
                swap_begin_to_presented_ms=summary([(r['presented_ns']-r['swap_begin_ns'])/1e6 for r in presented]),
                feedback_delivery_ms=summary([(r['received_ns']-r['presented_ns'])/1e6 for r in presented]),
                scope='viewer update token to host compositor-reported presentation; no source-frame or input-to-photon linkage',
                limitations=['Wayland debug audit adds CPU/protocol logging overhead; exclude these runs from FPS comparisons',
                             'Presented flags and clock identify compositor reporting, not independent optical proof',
                             'Discarded updates have no presented timestamp and are excluded from duration distributions',
                             'DMA-BUF inode/device identify reused storage, not a unique source frame'])


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--records',type=Path,required=True)
    parser.add_argument('--protocol-log',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    result=audit([json.loads(line) for line in args.records.read_text().splitlines()],args.protocol_log.read_text().splitlines())
    args.output.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result,indent=2))


if __name__=='__main__':main()

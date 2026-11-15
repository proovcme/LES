"""Storage grouping must precede top-k and retain independent source identities."""
import json
from collections import Counter
import pytest
from backend import native_core
from tests.test_light_edge_migration import binary


def test_bm25_groups_before_limit_with_global_scores_and_dataset_local_filenames(tmp_path):
    def call(action, **args):
        return native_core.required('bm25', request=dict(action=action, **args))
    database=str(tmp_path/'grouped.sqlite')
    handle=call('begin',database=database)['handle']
    rows=[dict(id=str(i),terms={'query':10},payload={'dataset_id':'a','file_name':'same.pdf','node_role':'evidence'}) for i in range(40)]
    rows.extend([dict(id='40',terms={'query':1,'other':30},payload={'dataset_id':'a','file_name':'other.pdf','node_role':'evidence'}),
        dict(id='41',terms={'query':1,'other':40},payload={'dataset_id':'b','file_name':'same.pdf','node_role':'evidence'})])
    call('put',handle=handle,points=rows)
    call('commit',handle=handle,revision='published',expected_count=len(rows))
    args=dict(database=database,revision='published',terms=['query'],limit=4,dataset_ids=['a','b'])
    assert all(int(i)<40 for i,_ in call('search',**args)['hits'])
    grouped=call('search',group_size=2,**args)['hits']
    assert {i for i,_ in grouped}.issuperset({'40','41'})
    scores=dict(call('search',**(args|{'limit':100}))['hits'])
    assert dict(grouped)==pytest.approx({i:scores[i] for i,_ in grouped})
    assert Counter(rows[int(i)]['payload']['dataset_id'] for i,_ in grouped)=={'a':3,'b':1}
    with pytest.raises(native_core.NativeCoreError,match='INDEX_SPARSE_STALE'):
        call('search',**(args|{'revision':'changed','group_size':2}))
    with pytest.raises(native_core.NativeCoreError,match='INDEX_SPARSE_REQUEST_INVALID'):
        call('search',**(args|{'group_size':0}))


def test_native_edge_groups_before_rrf_without_merging_dataset_local_names(binary,tmp_path):
    import hashlib
    from tests.test_light_edge_migration import source,complete,start,call
    path,manifest,rows=source(tmp_path,count=44)
    for i,row in enumerate(rows):
        row['payload'].update(dataset_id='a' if i<42 else 'b',
            file_name='same.pdf' if i<40 or i>=42 else 'other.pdf',
            node_role='evidence',text=f'query equipment capacity item {i}')
    path.write_text(''.join(json.dumps(r)+'\n' for r in rows),encoding='utf8')
    spec=json.loads(manifest.read_text());spec['source_sha256']=hashlib.sha256(path.read_bytes()).hexdigest()
    manifest.write_text(json.dumps(spec),encoding='utf8')
    target=tmp_path/'edge';code,_,error=complete(binary,'import',manifest,target)
    assert code==0,error
    owner=start(binary,'serve-stdio',target)
    try:
        query=dict(text='query equipment capacity',vector=[0.6,0.8,0.0],dense_name='dense',
            limit=8,rank_constant=60,fusion='rrf',overview=True,filters={'dataset_ids':['a','b']})
        reply=call(owner,'hybrid_search',query)
        found={(p['payload']['dataset_id'],p['payload']['file_name']) for p in reply['points']}
        assert found=={('a','same.pdf'),('a','other.pdf'),('b','same.pdf')}
        assert len(reply['points'])<=8
        assert all(p['payload']['source_sha256']=='a'*64 for p in reply['points'])
    finally:
        owner.stdin.close();owner.wait(timeout=10)

"""Differential security tests against the previous stdlib IP contract."""
import ipaddress
import random

from backend import trust_core


def reference(value, networks):
    try:
        address=ipaddress.ip_address(value)
    except ValueError:
        return False
    for network in networks:
        try:
            if address in ipaddress.ip_network(network, strict=False):
                return True
        except ValueError:
            pass
    return False


def test_native_networks_match_stdlib_on_ipv4_ipv6_masks_and_invalid_input():
    cases=[
        ('127.0.0.1',['127.0.0.1/32']), ('127.0.0.1',['::ffff:127.0.0.1/128']),
        ('::ffff:127.0.0.1',['127.0.0.1/32']), ('fe80::1%ethernet',['fe80::2%other/64']),
        ('fe80::1%ethernet/64',['::/0']), (' 127.0.0.1',['0.0.0.0/0']),
        ('127.0.0.1',['127.0.0.2/255.0.0.0']), ('127.0.0.1',['127.0.0.2/0.255.255.255']),
    ]
    for bad in ['', 'invalid','127.1','127.00.0.1','::1%','::1%one%two']:
        cases.append((bad,['0.0.0.0/0','::/0']))
    for bad in ['127.0.0.1/33','::1/129','127.0.0.1/-1','127.0.0.1/255.0.255.0','127.0.0.1/24/7']:
        cases.append(('127.0.0.1',[bad]))
    rng=random.Random(818)
    for width in (32,128):
        kind=ipaddress.IPv4Address if width==32 else ipaddress.IPv6Address
        for _ in range(250):
            address=kind(rng.getrandbits(width))
            bits=rng.randrange(width+1)
            net=ipaddress.ip_network(f'{address}/{bits}',strict=False)
            cases.append((str(address),[str(net)]))
            cases.append((str(kind(rng.getrandbits(width))),[str(net)]))
    for address, networks in cases:
        assert trust_core.ip_in_networks(address,networks)==reference(address,networks),(address,networks)


def test_foreign_forwarding_cannot_acquire_a_trusted_role():
    networks=['127.0.0.1/32','::1/128']
    for peer in ('192.0.2.1','2001:db8::1','invalid'):
        assert trust_core.resolve_client_ip(peer,'127.0.0.1, ::1','::1',networks)==peer
        assert not trust_core.proxy_asserts_network(peer,'true',networks,'admin')
        assert trust_core.network_role(peer,networks,'admin') is None
    assert trust_core.resolve_client_ip('::1','\u001c 192.0.2.1 \u001f, ::1',None,networks)=='192.0.2.1'
    assert trust_core.resolve_client_ip('::1',None,' 192.0.2.1 ',networks)=='192.0.2.1'
    assert trust_core.network_role('::1',networks,'admin')=='admin'
    for header in ('1','TRUE','yes','on','admin'):
        assert trust_core.proxy_asserts_network('127.0.0.1',header,networks,'admin')
    for header in (' true ','root','false','0'):
        assert not trust_core.proxy_asserts_network('127.0.0.1',header,networks,'admin')

package Shop::Cart;
use strict;
use warnings;
use Carp qw(croak);
use List::Util qw(sum0);

our $VERSION = '1.03';
use constant TAX => 0.19;    # VAT

# Create a cart.
sub new {
    my ($class, %args) = @_;
    my $self = { items => [], currency => $args{currency} // 'EUR', };
    return bless $self, $class;
}

sub add {
    my ($self, $sku, $qty, $price) = @_;
    croak "no sku" unless defined $sku;
    push @{ $self->{items} }, { sku => $sku, qty => $qty || 1, price => $price };
    return $self;
}

sub total {
    my $self = shift;
    my $net = sum0 map { $_->{qty} * $_->{price} } @{ $self->{items} };
    my $rate = $self->{currency} eq 'CHF' ? 0.081 : TAX;
    return sprintf '%.2f', $net * (1 + $rate);
}

sub clear {
    my $self = shift;
    $self->{items} = [];
    return $self;
}

sub summary {
    my $self = shift;
    my $lines = join "\n", map { "$_->{sku} x$_->{qty}" } @{ $self->{items} };
    return <<"END";
Cart ($self->{currency}):
$lines
END
}

1;

__END__

=head1 NAME

Shop::Cart - a shopping cart

=cut

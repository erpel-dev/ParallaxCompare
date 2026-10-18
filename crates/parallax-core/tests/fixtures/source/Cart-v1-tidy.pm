package Shop::Cart;

use strict;
use warnings;

use Carp      qw(croak);
use List::Util qw(sum0);

our $VERSION = "1.02";
use constant TAX => 0.19;

=head1 METHODS

=head2 new

Creates a cart.

=cut

sub new
{
    my ( $class, %args ) = @_;
    my $self = {
        items    => [],
        currency => $args{currency} // 'EUR'
    };
    return bless $self, $class;
}

sub add
{
    my ( $self, $sku, $qty, $price ) = @_;
    croak "no sku" unless defined $sku;
    push @{ $self->{items} },
      {
        sku   => $sku,
        qty   => $qty || 1,
        price => $price,
      };
    return $self;
}

sub total
{
    my $self = shift;
    my $net  = sum0 map { $_->{qty} * $_->{price} } @{ $self->{items} };
    return sprintf '%.2f', $net * ( 1 + TAX );
}

sub summary
{
    my $self  = shift;
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
